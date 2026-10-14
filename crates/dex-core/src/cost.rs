//! Persistent, provider-aware transcript usage (API-equivalent cost).
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, Seek, SeekFrom};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct ParserState {
    context: Option<AgentContext>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentContext {
    pub tokens: u64,
    pub window: Option<u64>,
    pub model: String,
    pub session_id: Option<String>,
    pub updated_at: Option<u64>,
}

fn model_window(model: &str) -> Option<u64> {
    match model {
        "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-fable-5-1" => Some(1_000_000),
        m if m.starts_with("claude-haiku-4-5") => Some(200_000),
        m if m.starts_with("muse-spark-1.") => Some(1_048_576),
        _ => None,
    }
}

struct UsageRow {
    key: String,
    model: String,
    ts: Option<u64>,
    tokens: Tokens,
}

fn parse_usage(v: &Value, engine: &str, state: &mut ParserState) -> Option<UsageRow> {
    if engine == "claude" && v["type"] == "system" && v["subtype"] == "compact_boundary" {
        state.context = None;
        return None;
    }
    if engine != "claude" || v["type"] != "assistant" {
        return None;
    }
    let m = &v["message"];
    let u = m.get("usage")?;
    let model = m["model"].as_str().unwrap_or("").to_string();
    // Last request occupancy: output is not part of that request's input context.
    state.context = Some(AgentContext {
        tokens: u["input_tokens"].as_u64().unwrap_or(0)
            + u["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        window: model_window(&model),
        model,
        updated_at: crate::chat::parse_ts(&v["timestamp"]),
        session_id: None,
    });
    Some(UsageRow {
        key: m["id"].as_str()?.into(),
        model: m["model"].as_str().unwrap_or("").into(),
        ts: crate::chat::parse_ts(&v["timestamp"]),
        tokens: Tokens {
            input: u["input_tokens"].as_u64().unwrap_or(0),
            output: u["output_tokens"].as_u64().unwrap_or(0),
            cache_read: u["cache_read_input_tokens"].as_u64().unwrap_or(0),
            cache_write: u["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        },
    })
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cost_sources (
    id INTEGER PRIMARY KEY, engine TEXT NOT NULL, session TEXT NOT NULL,
    path TEXT, byte_offset INTEGER NOT NULL DEFAULT 0, parser TEXT NOT NULL DEFAULT '{}',
    UNIQUE(engine, session)
);
CREATE TABLE IF NOT EXISTS cost_sessions (
    id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL REFERENCES cost_sources(id),
    agent_id TEXT NOT NULL, team_id TEXT NOT NULL, delegation_id INTEGER,
    provider TEXT NOT NULL, model TEXT NOT NULL, started_at INTEGER NOT NULL, ended_at INTEGER
);
CREATE INDEX IF NOT EXISTS cost_sessions_team ON cost_sessions(team_id, source_id);
CREATE INDEX IF NOT EXISTS cost_sessions_agent ON cost_sessions(agent_id, ended_at);
CREATE INDEX IF NOT EXISTS cost_sessions_source ON cost_sessions(source_id, started_at);
CREATE INDEX IF NOT EXISTS cost_sessions_delegation ON cost_sessions(delegation_id);
CREATE TABLE IF NOT EXISTS cost_messages (
    source_id INTEGER NOT NULL REFERENCES cost_sources(id), message_key TEXT NOT NULL,
    segment_id INTEGER NOT NULL REFERENCES cost_sessions(id), model TEXT NOT NULL, ts INTEGER NOT NULL,
    input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
    PRIMARY KEY(source_id, message_key)
);
CREATE INDEX IF NOT EXISTS cost_messages_segment ON cost_messages(segment_id, ts);
";

// Additive, rerunnable; rollback: DROP TABLE IF EXISTS cost_context_heads;
// Usage history is independent and remains intact on rollback.
pub(crate) const CONTEXT_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS cost_context_heads (
    agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    source_id INTEGER REFERENCES cost_sources(id),
    snapshot TEXT
);
";

pub(crate) fn reset_context(c: &Connection, agent: &str) -> Result<(), String> {
    c.execute(
        "INSERT INTO cost_context_heads(agent_id,source_id) VALUES(?1,NULL)
        ON CONFLICT(agent_id) DO UPDATE SET source_id=NULL",
        [agent],
    )
    .map_err(err)?;
    Ok(())
}

/// Lock-free unit of [`context`]: everything the file read needs, resolved with fast queries.
/// `source` (id + session) survives even when no transcript file resolves — the snapshot still
/// names the session, only the ingest is skipped.
struct ContextPre {
    team: String,
    model: String,
    /// Raw `cost_context_heads.source_id` read here; `context_post` publishes only if unchanged.
    head: Option<i64>,
    source: Option<(i64, String)>,
    ingest: Option<IngestPre>,
}

struct IngestPre {
    engine: String,
    path: std::path::PathBuf,
    offset: u64,
    state: ParserState,
    force: bool,
}

fn context_pre(c: &Connection, agent: &str) -> Result<ContextPre, String> {
    let a = crate::teams::get_agent(c, agent)?;
    // Upgrade existing installations lazily, using the indexed agent history.
    c.execute(
        "INSERT OR IGNORE INTO cost_context_heads(agent_id,source_id) VALUES(?1,
        (SELECT source_id FROM cost_sessions WHERE agent_id=?1 ORDER BY id DESC LIMIT 1))",
        [agent],
    )
    .map_err(err)?;
    let head: Option<i64> = c
        .query_row("SELECT source_id FROM cost_context_heads WHERE agent_id=?1", [agent], |r| r.get(0))
        .map_err(err)?;
    let model = a.active_model.unwrap_or(a.model);
    // Only Claude transcripts are read; older engines' sources keep their history but are not re-read.
    let source = match head {
        Some(id) => c
            .query_row("SELECT engine,session,path FROM cost_sources WHERE id=?1 AND engine='claude'", [id], |r| {
                Ok((id, r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
            })
            .optional()
            .map_err(err)?,
        None => None,
    };
    let mut ingest = None;
    let mut source_id = None;
    if let Some((source, engine, session, known)) = source {
        source_id = Some((source, session.clone()));
        if let Some(path) = crate::chat::transcript_of(Some(&session), known.as_deref()) {
            let (offset, state, force) = ingest_begin(c, source)?;
            ingest = Some(IngestPre { engine, path, offset, state, force });
        }
    }
    Ok(ContextPre { team: a.team_id, model, head, source: source_id, ingest })
}

/// Snapshot from the (possibly concurrently advanced) parser row; stores it on change.
fn context_post(c: &Connection, agent: &str, pre: &ContextPre) -> Result<Option<(AgentContext, bool)>, String> {
    let (source, session) = pre.source.clone().unzip();
    let mut out = AgentContext { window: model_window(&pre.model), model: pre.model.clone(), ..Default::default() };
    if let (Some(source), Some(session)) = (source, session) {
        let parser: String = c
            .query_row("SELECT parser FROM cost_sources WHERE id=?1", [source], |r| r.get(0))
            .map_err(err)?;
        let state: ParserState = serde_json::from_str(&parser).map_err(err)?;
        if let Some(snapshot) = state.context {
            out = snapshot;
        }
        out.session_id = Some(session);
    }
    // Baş, kilit dışı okuma sırasında taze başlatmayla değişmiş olabilir: aynı kilit altında
    // yeniden oku; kaynak artık aynı değilse bu ölçüm bayattır, yazma ve yayınlama.
    let Some((head, previous)) = c
        .query_row(
            "SELECT source_id,snapshot FROM cost_context_heads WHERE agent_id=?1",
            [agent],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(err)?
    else {
        return Ok(None);
    };
    if head != pre.head {
        return Ok(None);
    }
    let encoded = serde_json::to_string(&out).map_err(err)?;
    let changed = previous.as_deref() != Some(encoded.as_str());
    if changed {
        c.execute(
            "UPDATE cost_context_heads SET snapshot=?2 WHERE agent_id=?1",
            params![agent, encoded],
        )
        .map_err(err)?;
    }
    Ok(Some((out, changed)))
}

#[cfg(test)]
fn context(c: &Connection, agent: &str) -> Result<(AgentContext, bool), String> {
    let pre = context_pre(c, agent)?;
    if let (Some((source, _)), Some(ig)) = (pre.source.as_ref(), pre.ingest.as_ref()) {
        let mut reader = TailReader::open(*source, &ig.path, ig.offset)?;
        let mut state = ig.state.clone();
        loop {
            let Some((lines, pos)) = reader.next_chunk()? else { break };
            let base = pos - lines.iter().map(|l| l.len() as u64).sum::<u64>();
            let (rows, next) = parse_chunk(*source, &ig.engine, &lines, state, base)?;
            state = next;
            commit_chunk(c, *source, &ig.path, &rows, &state, base, pos, ig.force)?;
        }
    }
    context_post(c, agent, &pre)?.ok_or_else(|| "bağlam başı işlem sırasında değişti".to_string())
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub(crate) fn end_agent(c: &Connection, agent: &str) -> Result<(), String> {
    c.execute(
        "UPDATE cost_sessions SET ended_at=?2 WHERE agent_id=?1 AND ended_at IS NULL",
        params![agent, now()],
    )
    .map_err(err)?;
    Ok(())
}

/// Capture attribution at hooks, even before a transcript becomes resumable. Never FK to
/// agents/delegations: deleting an office row must not erase already measured usage.
pub(crate) fn observe(c: &Connection, agent: &str, session: &str, engine: &str, path: Option<&str>, event: &str) -> Result<(), String> {
    let a = crate::teams::get_agent(c, agent)?;
    let provider = a.active_provider.unwrap_or(a.provider);
    let model = a.active_model.unwrap_or(a.model);
    let delegation: Option<i64> = c.query_row(
        "SELECT id FROM delegations WHERE to_agent=?1 AND (status='running' OR (status='queued' AND ?2)) ORDER BY status='running' DESC,id LIMIT 1",
        params![agent,matches!(event,"SessionStart"|"UserPromptSubmit")], |r| r.get(0)).optional().map_err(err)?;
    let tx = c.unchecked_transaction().map_err(err)?;
    tx.execute(
        "INSERT INTO cost_sources(engine,session,path) VALUES(?1,?2,?3)
        ON CONFLICT(engine,session) DO UPDATE SET path=COALESCE(excluded.path,cost_sources.path)",
        params![engine, session, path],
    )
    .map_err(err)?;
    let source: i64 = tx
        .query_row(
            "SELECT id FROM cost_sources WHERE engine=?1 AND session=?2",
            params![engine, session],
            |r| r.get(0),
        )
        .map_err(err)?;
    type Active = (i64, i64, Option<i64>, String, String);
    let active: Option<Active> = tx.query_row(
        "SELECT id,source_id,delegation_id,provider,model FROM cost_sessions WHERE agent_id=?1 AND ended_at IS NULL ORDER BY id DESC LIMIT 1",
        [agent],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(err)?;
    // Reporting finishes the DB job before its CLI flushes the final reply. Retain that
    // attribution until a new delegation/session actually starts.
    let delegation = delegation.or_else(|| active.as_ref().filter(|s| s.1 == source).and_then(|s| s.2));
    let same = active
        .as_ref()
        .is_some_and(|s| s.1 == source && s.2 == delegation && s.3 == provider && s.4 == model);
    let time = now();
    if !same {
        tx.execute(
            "UPDATE cost_sessions SET ended_at=?2 WHERE agent_id=?1 AND ended_at IS NULL",
            params![agent, time],
        )
        .map_err(err)?;
        tx.execute(
            "INSERT INTO cost_sessions(source_id,agent_id,team_id,delegation_id,provider,model,started_at)
            VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![source, agent, a.team_id, delegation, provider, model, time],
        )
        .map_err(err)?;
    }
    if event == "SessionEnd" {
        tx.execute(
            "UPDATE cost_sessions SET ended_at=?2 WHERE agent_id=?1 AND source_id=?3 AND ended_at IS NULL",
            params![agent, time, source],
        )
        .map_err(err)?;
    }
    tx.execute(
        "INSERT INTO cost_context_heads(agent_id,source_id) VALUES(?1,?2)
        ON CONFLICT(agent_id) DO UPDATE SET source_id=excluded.source_id",
        params![agent, source],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

#[derive(Deserialize)]
struct StoredState {
    #[serde(default)]
    context: Option<AgentContext>,
}

/// Complete lines only. Cursor, parser state and message upserts commit together; a crash
/// cannot count an append twice. Message content is neither persisted nor logged.
///
/// #1813: the transcript file used to be read line by line while holding the process-wide DB
/// mutex (every hook runs `agent_context`, `usage_summary` ingests every source). A large tail —
/// or a slow `agent-context` emit in the same lock — held the lock for seconds, and the next
/// hook's PreToolUse decision queued behind it until curl's 8 s timeout denied the tool.
/// The ingest is therefore split: [`ingest_begin`] (fast DB reads), [`TailReader`] + [`parse_chunk`]
/// (file I/O and JSON parsing with no lock held), [`commit_chunk`] (one short transaction per
/// chunk, so hook decisions slip between chunks). Chunk upserts stay idempotent
/// (`ON CONFLICT … MAX`, message keys) and the offset only advances over committed complete
/// lines, never backwards (`max`), so concurrent ingests of the same source cannot double-count.
const INGEST_CHUNK: usize = 500;

/// File read + parse budget above which the ingest is worth a diagnostic line (durations and
/// counts only, no content). The slow tail blocks no hook, but it explains long hook gaps nearby.
const INGEST_SLOW_MS: u64 = 500;

/// Fast DB reads only: cursor + parser state. No file I/O, no transaction. `force` is set when
/// a pre-context cursor is replayed from zero (parser reset): the replay is authoritative and the
/// commit must overwrite unconditionally, even though the stored cursor already covers the file.
fn ingest_begin(c: &Connection, source: i64) -> Result<(u64, ParserState, bool), String> {
    let (offset, state): (u64, String) = c
        .query_row("SELECT byte_offset,parser FROM cost_sources WHERE id=?1", [source], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|_| format!("usage source {source}: kaynak yok"))?;
    let saved: StoredState = serde_json::from_str(&state).map_err(err)?;
    // Pre-context cursors get one replay. Existing message keys make it idempotent.
    // Afterwards even a null context is persisted.
    let legacy = !serde_json::from_str::<Value>(&state)
        .map_err(err)?
        .as_object()
        .is_some_and(|o| o.contains_key("context"));
    if legacy {
        Ok((0, ParserState::default(), true))
    } else {
        Ok((offset, ParserState { context: saved.context }, false))
    }
}

/// A transcript tail read with no DB access: at most `INGEST_CHUNK` complete `\n`-terminated
/// lines per call, the byte offset past them. A trailing partial line stays for the next read.
struct TailReader {
    source: i64,
    reader: std::io::BufReader<std::fs::File>,
    pos: u64,
}

impl TailReader {
    fn open(source: i64, path: &std::path::Path, offset: u64) -> Result<Self, String> {
        let mut f =
            std::fs::File::open(path).map_err(|_| format!("usage source {source}: transcript okunamıyor"))?;
        if f.metadata().map_err(err)?.len() < offset {
            return Err(format!("usage source {source}: transcript küçüldü"));
        }
        f.seek(SeekFrom::Start(offset)).map_err(err)?;
        Ok(Self { source, reader: std::io::BufReader::new(f), pos: offset })
    }

    fn next_chunk(&mut self) -> Result<Option<(Vec<String>, u64)>, String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        while lines.len() < INGEST_CHUNK {
            line.clear();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|_| format!("usage source {}: satır okunamıyor", self.source))?;
            if n == 0 || !line.ends_with('\n') {
                break;
            }
            self.pos += n as u64;
            lines.push(std::mem::take(&mut line));
        }
        if lines.is_empty() {
            Ok(None)
        } else {
            Ok(Some((lines, self.pos)))
        }
    }
}

/// Pure CPU: JSON parsing and usage-row extraction of one chunk. No DB access.
fn parse_chunk(
    source: i64,
    engine: &str,
    lines: &[String],
    mut state: ParserState,
    base: u64,
) -> Result<(Vec<UsageRow>, ParserState), String> {
    let mut rows = Vec::new();
    let mut at = base;
    for line in lines {
        // `read_line` strips nothing here: `lines` already came from `TailReader`.
        let v: Value =
            serde_json::from_str(line).map_err(|_| format!("usage source {source}: geçersiz JSON, offset {at}"))?;
        if let Some(row) = parse_usage(&v, engine, &mut state) {
            rows.push(row);
        }
        at += line.len() as u64;
    }
    Ok((rows, state))
}

/// One short transaction: the chunk's upserts plus the cursor advance. `base` is the offset
/// this ingest read from, `force` a replay from zero after a parser reset; the stored cursor can
/// only have moved forward since:
/// - force, or stored <= base: nobody covered our range — unconditional advance (legacy replay
///   included, even though the stored cursor already spans the file);
/// - stored in (base, pos): our tail is partly new — rows apply idempotently, the newer parser wins;
/// - stored >= pos: our range was already covered — rows re-apply harmlessly, cursor untouched.
/// Rows are idempotent either way (`ON CONFLICT … MAX`, message keys), so concurrent ingests of
/// the same source cannot double-count; only the cursor needs the guard.
fn commit_chunk(
    c: &Connection,
    source: i64,
    path: &std::path::Path,
    rows: &[UsageRow],
    state: &ParserState,
    base: u64,
    pos: u64,
    force: bool,
) -> Result<(), String> {
    let tx = c.unchecked_transaction().map_err(err)?;
    for row in rows {
        // Missing timestamps cannot be reliably assigned across resumed/provider-switched sessions.
        let ts = row.ts.ok_or_else(|| format!("usage source {source}: usage zaman damgası eksik"))?;
        let segment: Option<i64> = tx
            .query_row(
                "SELECT id FROM cost_sessions WHERE source_id=?1 AND started_at<=?2
            AND (ended_at IS NULL OR ended_at>=?2) ORDER BY started_at DESC,id DESC LIMIT 1",
                params![source, ts],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        if let Some(segment) = segment {
            tx.execute(
                "INSERT INTO cost_messages(source_id,message_key,segment_id,model,ts,input,output,cache_read,cache_write)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                ON CONFLICT(source_id,message_key) DO UPDATE SET
                input=MAX(input,excluded.input),output=MAX(output,excluded.output),
                cache_read=MAX(cache_read,excluded.cache_read),cache_write=MAX(cache_write,excluded.cache_write)",
                params![
                    source,
                    row.key,
                    segment,
                    row.model,
                    ts,
                    row.tokens.input,
                    row.tokens.output,
                    row.tokens.cache_read,
                    row.tokens.cache_write
                ],
            )
            .map_err(err)?;
        }
    }
    tx.execute(
        "UPDATE cost_sources SET byte_offset=CASE WHEN ?6 OR byte_offset<=?5 THEN ?2 ELSE max(byte_offset,?2) END,
        parser=CASE WHEN ?6 OR byte_offset<=?5 THEN ?3 WHEN ?2>byte_offset THEN ?3 ELSE parser END,path=?4 WHERE id=?1",
        params![source, pos, serde_json::to_string(state).map_err(err)?, path.to_string_lossy(), base, force as i64],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

/// Compatibility path (tests, single-lock callers): same behavior as the old monolithic ingest,
/// still under the caller's lock. Production paths use [`ingest_split`] so the lock is released
/// between chunks.
#[cfg(test)]
fn ingest(c: &Connection, source: i64, engine: &str, path: &std::path::Path) -> Result<(), String> {
    let (offset, state, force) = ingest_begin(c, source)?;
    let mut reader = TailReader::open(source, path, offset)?;
    let mut state = state;
    loop {
        let Some((lines, pos)) = reader.next_chunk()? else { break };
        let base = pos - lines.iter().map(|l| l.len() as u64).sum::<u64>();
        let (rows, next) = parse_chunk(source, engine, &lines, state, base)?;
        state = next;
        commit_chunk(c, source, path, &rows, &state, base, pos, force)?;
    }
    Ok(())
}

/// Production path: the DB lock is held only for the fast cursor read and for one short
/// transaction per chunk. File I/O and JSON parsing run lock-free, so hook decisions
/// (PreToolUse) never queue behind a transcript tail.
fn ingest_split(db: &crate::db::Db, source: i64, engine: &str, path: &std::path::Path) -> Result<(), String> {
    let started = std::time::Instant::now();
    let (offset, state, force) = crate::db::with(db, |c| ingest_begin(c, source))?;
    let mut reader = TailReader::open(source, path, offset)?;
    let mut state = state;
    let mut lines_total: u64 = 0;
    loop {
        let Some((lines, pos)) = reader.next_chunk()? else { break };
        lines_total += lines.len() as u64;
        let base = pos - lines.iter().map(|l| l.len() as u64).sum::<u64>();
        let (rows, next) = parse_chunk(source, engine, &lines, state, base)?;
        state = next;
        crate::db::with(db, |c| commit_chunk(c, source, path, &rows, &state, base, pos, force))?;
    }
    let ms = started.elapsed().as_millis() as u64;
    if ms > INGEST_SLOW_MS {
        // Counts and durations only; message content is never logged.
        crate::diagnostics::event(
            "cost_ingest_slow",
            serde_json::json!({"id": source, "line": lines_total, "duration_ms": ms, "bytes": reader.pos.saturating_sub(offset)}),
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Total {
    pub tokens: Tokens,
    pub usd: Option<f64>,
}
impl Default for Total {
    fn default() -> Self {
        Self {
            tokens: Tokens::default(),
            usd: Some(0.0),
        }
    }
}
impl Total {
    fn add(&mut self, t: Tokens, usd: Option<f64>) {
        self.tokens.input += t.input;
        self.tokens.output += t.output;
        self.tokens.cache_read += t.cache_read;
        self.tokens.cache_write += t.cache_write;
        self.usd = self.usd.zip(usd).map(|(a, b)| a + b);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub cost_label: &'static str,
    pub total: Total,
    pub agents: BTreeMap<String, Total>,
    pub delegations: BTreeMap<i64, Total>,
    pub warnings: Vec<String>,
}

/// #1813: `usage_summary` eskiden bütün kaynakları tek DB kilidi altında ingest ediyordu.
/// Kaynak listesi kısa bir kilitle alınır, her kaynak [`ingest_split`] ile (dosya okuma kilitsiz,
/// parça başı kısa işlem) işlenir, toplama ayrı bir kısa kilitte koşar. Sıra korunur: her kaynağın
/// uyarısı (ingest hatası ya da bulunamayan transcript) sorgu sırasındaki yerinde durur.
enum SourceItem {
    Ready { id: i64, engine: String, path: std::path::PathBuf },
    Missing { warning: String },
}

fn source_files(c: &Connection, team: &str, delegation: Option<i64>) -> Result<Vec<SourceItem>, String> {
    let sources = c
        .prepare(
            "SELECT DISTINCT f.id,f.engine,f.session,f.path FROM cost_sources f
        JOIN cost_sessions s ON s.source_id=f.id
        WHERE f.engine='claude' AND s.team_id=?1 AND (?2 IS NULL OR s.delegation_id=?2)",
        )
        .map_err(err)?
        .query_map(params![team, delegation], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let mut out = Vec::new();
    for (id, engine, session, known) in sources {
        match crate::chat::transcript_of(Some(&session), known.as_deref()) {
            Some(path) => out.push(SourceItem::Ready { id, engine, path }),
            None => out.push(SourceItem::Missing {
                warning: format!("usage source {id}: transcript bulunamadı"),
            }),
        }
    }
    Ok(out)
}

#[cfg(test)]
fn summary(c: &Connection, team: &str, since: Option<i64>, delegation: Option<i64>) -> Result<Summary, String> {
    let files = source_files(c, team, delegation)?;
    let mut warnings = Vec::new();
    for item in &files {
        match item {
            SourceItem::Ready { id, engine, path } => {
                if let Err(e) = ingest(c, *id, engine, path) {
                    warnings.push(e);
                }
            }
            SourceItem::Missing { warning } => warnings.push(warning.clone()),
        }
    }
    summary_aggregate(c, team, since, delegation, warnings)
}

fn summary_aggregate(
    c: &Connection,
    team: &str,
    since: Option<i64>,
    delegation: Option<i64>,
    warnings: Vec<String>,
) -> Result<Summary, String> {
    let mut out = Summary {
        cost_label: "API eşdeğer maliyeti",
        total: Total::default(),
        agents: BTreeMap::new(),
        delegations: BTreeMap::new(),
        warnings,
    };
    let price_map: BTreeMap<_, _> = prices(c)?.into_iter().map(|p| ((p.provider, p.model), p.rates)).collect();
    let mut st = c
        .prepare(
            "SELECT s.agent_id,s.delegation_id,SUM(m.input),SUM(m.output),SUM(m.cache_read),SUM(m.cache_write),s.provider,m.model
        FROM cost_messages m JOIN cost_sessions s ON s.id=m.segment_id
        WHERE s.team_id=?1 AND (?2 IS NULL OR m.ts>=?2) AND (?3 IS NULL OR s.delegation_id=?3)
        GROUP BY s.agent_id,s.delegation_id,s.provider,m.model",
        )
        .map_err(err)?;
    let rows = st
        .query_map(params![team, since, delegation], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                Tokens {
                    input: r.get(2)?,
                    output: r.get(3)?,
                    cache_read: r.get(4)?,
                    cache_write: r.get(5)?,
                },
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })
        .map_err(err)?;
    for row in rows {
        let (agent, job, tokens, provider, model) = row.map_err(err)?;
        let usd = price_map.get(&(provider, model)).and_then(|p| *p).map(|r| {
            (tokens.input as f64 * r.input
                + tokens.output as f64 * r.output
                + tokens.cache_read as f64 * r.cache_read
                + tokens.cache_write as f64 * r.cache_write)
                / 1_000_000.0
        });
        out.total.add(tokens, usd);
        out.agents.entry(agent).or_default().add(tokens, usd);
        if let Some(job) = job {
            out.delegations.entry(job).or_default().add(tokens, usd);
        }
    }
    Ok(out)
}

/// USD per million tokens. No guessed list prices: catalog entries default to unknown.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Price {
    pub provider: String,
    pub model: String,
    pub rates: Option<Rates>,
}
fn prices(c: &Connection) -> Result<Vec<Price>, String> {
    let mut table: BTreeMap<(String, String), Option<Rates>> = crate::providers::PROVIDERS
        .iter()
        .flat_map(|p| crate::providers::models(p).into_iter().map(move |m| ((p.id.to_string(), m.id), None)))
        .collect();
    // Models seen only in transcripts are listed without inventing prices.
    let mut st = c
        .prepare("SELECT DISTINCT s.provider,m.model FROM cost_messages m JOIN cost_sessions s ON s.id=m.segment_id")
        .map_err(err)?;
    for r in st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(err)?
    {
        table.entry(r.map_err(err)?).or_insert(None);
    }
    let stored: Option<String> = c
        .query_row("SELECT value FROM settings WHERE key='usage_prices'", [], |r| r.get(0))
        .optional()
        .map_err(err)?;
    if let Some(stored) = stored {
        for p in serde_json::from_str::<Vec<Price>>(&stored).map_err(err)? {
            table.insert((p.provider, p.model), p.rates);
        }
    }
    Ok(table
        .into_iter()
        .map(|((provider, model), rates)| Price { provider, model, rates })
        .collect())
}
fn set_prices(c: &Connection, values: Vec<Price>) -> Result<Vec<Price>, String> {
    let mut keys = std::collections::HashSet::new();
    for p in &values {
        if p.provider.trim().is_empty() || p.model.trim().is_empty() || !keys.insert((&p.provider, &p.model)) {
            return Err("fiyat sağlayıcı/model anahtarı boş veya yinelenmiş".into());
        }
        if p.rates.is_some_and(|r| {
            [r.input, r.output, r.cache_read, r.cache_write]
                .iter()
                .any(|n| !n.is_finite() || *n < 0.0)
        }) {
            return Err("fiyatlar sonlu ve negatif olmayan $/Mtok olmalı".into());
        }
    }
    c.execute(
        "INSERT INTO settings(key,value) VALUES('usage_prices',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [serde_json::to_string(&values).map_err(err)?],
    )
    .map_err(err)?;
    prices(c)
}

impl crate::Core {
    pub fn agent_context(&self, agent_id: &str) -> Result<AgentContext, String> {
        // #1813: kısa kilitler — dosya okuma/JSON çözümleme kilitsiz, her parça kısa bir işlem.
        // Böylece bir transcript kuyruğu işlenirken PreToolUse kararları kilidi aralarda alır.
        // Kilit dışındayken taze başlatma (`reset_context`/yeni SessionStart) başı değiştirebilir;
        // o zaman eski ölçüm ne yazılır ne yayınlanır, güncel başla yeniden hesaplanır.
        let mut attempts = 0;
        let (pre, snapshot, changed) = loop {
            let pre = crate::db::with(&self.db, |c| context_pre(c, agent_id))?;
            if let (Some((source, _)), Some(ig)) = (pre.source.as_ref(), pre.ingest.as_ref()) {
                let mut reader = TailReader::open(*source, &ig.path, ig.offset)?;
                let mut state = ig.state.clone();
                loop {
                    let Some((lines, pos)) = reader.next_chunk()? else { break };
                    let base = pos - lines.iter().map(|l| l.len() as u64).sum::<u64>();
                    let (rows, next) = parse_chunk(*source, &ig.engine, &lines, state, base)?;
                    state = next;
                    crate::db::with(&self.db, |c| commit_chunk(c, *source, &ig.path, &rows, &state, base, pos, ig.force))?;
                }
            }
            match crate::db::with(&self.db, |c| context_post(c, agent_id, &pre))? {
                Some((snapshot, changed)) => break (pre, snapshot, changed),
                None if attempts < 2 => attempts += 1,
                None => return Err("bağlam başı hesaplama sırasında değişmeye devam etti".into()),
            }
        };
        if changed {
            // Kilit dışında: yavaş bir tüketici DB kilidini tutmasın. #1813'ün kök nedeni buydu:
            // her hook bu emit'i kilit altında yapıyor, sonraki PreToolUse 8 sn'yi aşıp
            // fail-closed ile reddediliyordu.
            let _ = self.events.emit(
                "agent-context",
                &serde_json::json!({
                    "agentId":agent_id,"teamId":pre.team,"context":snapshot
                }),
            );
        }
        Ok(snapshot)
    }

    pub fn team_context(&self, team_id: &str) -> Result<BTreeMap<String, AgentContext>, String> {
        let ids = crate::db::with(&self.db, |c| {
            c.prepare("SELECT id FROM agents WHERE team_id=?1")
                .map_err(err)?
                .query_map([team_id], |r| r.get::<_, String>(0))
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)
        })?;
        ids.into_iter().map(|id| self.agent_context(&id).map(|s| (id, s))).collect()
    }

    pub fn usage_summary(&self, team_id: &str, since: Option<i64>) -> Result<Summary, String> {
        if since.is_some_and(|t| t < 0) {
            return Err("since negatif olamaz (epoch ms)".into());
        }
        // #1813: tek kilit yok — kaynaklar kilitsiz ingest ile, toplama ayrı kısa kilitte.
        let files = crate::db::with(&self.db, |c| source_files(c, team_id, None))?;
        let mut warnings = Vec::new();
        for item in &files {
            match item {
                SourceItem::Ready { id, engine, path } => {
                    if let Err(e) = ingest_split(&self.db, *id, engine, path) {
                        warnings.push(e);
                    }
                }
                SourceItem::Missing { warning } => warnings.push(warning.clone()),
            }
        }
        crate::db::with(&self.db, |c| summary_aggregate(c, team_id, since, None, warnings))
    }
    pub fn usage_delegation(&self, id: i64) -> Result<Summary, String> {
        // The historical record survives deletion of the original delegation/team.
        let (team, files) = crate::db::with(&self.db, |c| {
            let team: Option<String> = c
                .query_row("SELECT team_id FROM cost_sessions WHERE delegation_id=?1 LIMIT 1", [id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(err)?;
            let team = match team {
                Some(t) => t,
                None => {
                    let d = crate::delegate::get(c, id)?;
                    d.service_team_id.unwrap_or(d.team_id)
                }
            };
            Ok((team.clone(), source_files(c, &team, Some(id))?))
        })?;
        let mut warnings = Vec::new();
        for item in &files {
            match item {
                SourceItem::Ready { id: source, engine, path } => {
                    if let Err(e) = ingest_split(&self.db, *source, engine, path) {
                        warnings.push(e);
                    }
                }
                SourceItem::Missing { warning } => warnings.push(warning.clone()),
            }
        }
        crate::db::with(&self.db, |c| summary_aggregate(c, &team, None, Some(id), warnings))
    }
    pub fn settings_usage_prices(&self) -> Result<Vec<Price>, String> {
        crate::db::with(&self.db, prices)
    }
    pub fn settings_set_usage_prices(&self, prices: Vec<Price>) -> Result<Vec<Price>, String> {
        crate::db::with(&self.db, |c| set_prices(c, prices))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_hook_resets_session_and_emits_only_changes_with_leader_cost() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<Value>>>);
        impl crate::EventSink for Sink {
            fn emit(&self, name: &str, value: Value) -> Result<(), String> {
                if name == "agent-context" {
                    self.0.lock().unwrap().push(value);
                }
                Ok(())
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../.dex/tmp/context-{}", std::process::id()));
        let sink = Sink::default();
        let core = crate::Core::start(
            crate::Paths {
                data_dir: root.join("data"),
                config_dir: root.join("config"),
            },
            crate::Events::new(sink.clone()),
        )
        .unwrap();
        let path = root.join("context.jsonl");
        let ts = "2099-10-05T10:00:00Z";
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({"type":"assistant","timestamp":ts,
            "message":{"id":"leader-m1","model":"claude-opus-5-5","usage":{"input_tokens":800,"output_tokens":10}}})
            ),
        )
        .unwrap();
        core.session_on_hook("timur", "SessionStart", "context-old", Some(path.to_str().unwrap()), Some("claude"));
        assert_eq!(sink.0.lock().unwrap().last().unwrap()["context"]["tokens"], 800);
        let first = core.agent_context("timur").unwrap();
        assert_eq!(first.tokens, 800);
        let count = sink.0.lock().unwrap().len();
        assert_eq!(core.agent_context("timur").unwrap(), first);
        assert_eq!(sink.0.lock().unwrap().len(), count);
        let summary = core.usage_summary("default", None).unwrap();
        assert_eq!(summary.agents["timur"].tokens.input, 800);
        assert!(summary.delegations.is_empty());
        core.session_on_hook("timur", "SessionStart", "context-clear", None, Some("claude"));
        let cleared = core.agent_context("timur").unwrap();
        assert_eq!(cleared.tokens, 0);
        assert_eq!(cleared.session_id.as_deref(), Some("context-clear"));
        crate::db::with(&core.db, |c| reset_context(c, "timur")).unwrap();
        assert_eq!(core.agent_context("timur").unwrap().session_id, None);
        assert_eq!(core.usage_summary("default", None).unwrap().agents["timur"].tokens.input, 800);
        crate::db::with(&core.db, |c| {
            let before: i64 = c.query_row("SELECT count(*) FROM cost_messages", [], |r| r.get(0)).map_err(err)?;
            c.execute_batch(CONTEXT_SCHEMA).map_err(err)?;
            c.execute_batch(CONTEXT_SCHEMA).map_err(err)?;
            assert_eq!(
                before,
                c.query_row("SELECT count(*) FROM cost_messages", [], |r| r.get::<_, i64>(0))
                    .unwrap()
            );
            assert_eq!(context(c, "timur")?.0.session_id, None);
            Ok(())
        })
        .unwrap();
        core.shutdown();
    }

    /// #1816: eski hook'un ölçümü kilit dışındayken taze başlatma başı sıfırlar; eski
    /// `context_post` yeni oturumun snapshot'ını ezmemeli ve yayın için `changed` vermemeli.
    #[test]
    fn stale_context_post_after_reset_neither_writes_nor_publishes() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        observe(&c, "timur", "stale-old", "claude", None, "SessionStart").unwrap();
        let old = context_pre(&c, "timur").unwrap();
        assert_eq!(old.source.as_ref().map(|s| s.1.as_str()), Some("stale-old"));
        reset_context(&c, "timur").unwrap();
        let fresh = context(&c, "timur").unwrap().0;
        assert_eq!(fresh.session_id, None);
        assert_eq!(context_post(&c, "timur", &old).unwrap(), None);
        let stored: String = c
            .query_row("SELECT snapshot FROM cost_context_heads WHERE agent_id='timur'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(serde_json::from_str::<AgentContext>(&stored).unwrap(), fresh);
        // Aynı baş ve aynı değer: geçerli, ama yeniden yayınlanmaz.
        let same = context_pre(&c, "timur").unwrap();
        assert_eq!(context_post(&c, "timur", &same).unwrap(), Some((fresh, false)));
    }

    #[test]
    fn context_compaction_invalidates_old_measurement() {
        let mut state = ParserState {
            context: Some(AgentContext {
                tokens: 1234,
                ..Default::default()
            }),
        };
        parse_usage(&serde_json::json!({"type":"system","subtype":"compact_boundary"}), "claude", &mut state);
        assert!(state.context.is_none());
    }

    #[test]
    fn context_upgrade_replays_old_cursor_without_double_billing() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("context-upgrade-{}.jsonl", std::process::id()));
        std::fs::write(&path,concat!(r#"{"type":"assistant","timestamp":"2026-10-05T10:00:00Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":45,"output_tokens":4}}}"#,"\n")).unwrap();
        observe(&c, "timur", "upgrade", "claude", Some(path.to_str().unwrap()), "SessionStart").unwrap();
        c.execute("UPDATE cost_sessions SET started_at=0", []).unwrap();
        assert_eq!(summary(&c, "default", None, None).unwrap().total.tokens.input, 45);
        c.execute("UPDATE cost_sources SET parser='{}'", []).unwrap();
        let snapshot = context(&c, "timur").unwrap().0;
        assert_eq!(snapshot.tokens, 45);
        assert_eq!(summary(&c, "default", None, None).unwrap().total.tokens.input, 45);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn context_tracks_latest_request_not_cumulative_cost() {
        let mut state = ParserState::default();
        for input in [900, 12] {
            parse_usage(
                &serde_json::json!({"type":"assistant","timestamp":"2026-10-05T10:00:00Z",
                "message":{"id":"m","model":"claude-opus-5-5","usage":{
                    "input_tokens":input,"cache_read_input_tokens":20,"cache_creation_input_tokens":3,"output_tokens":90}}}),
                "claude",
                &mut state,
            );
            let saved = serde_json::to_value(&state).unwrap();
            assert_eq!(saved["context"]["tokens"], input + 23);
            assert_eq!(saved["context"]["window"], 1_000_000);
        }
    }

    /// #1813: yavaş bir `agent-context` tüketicisi sürerken PreToolUse kararı hızlı döner.
    /// Eskiden emit DB kilidi altında yapılıyordu; 2 sn'lik tüketici kararı da 2 sn bekletiyor,
    /// 8 sn'lik curl sınırında fail-closed red geliyordu.
    #[test]
    fn slow_context_emit_does_not_block_pre_tool_decision() {
        struct Slow;
        impl crate::EventSink for Slow {
            fn emit(&self, event: &str, _: Value) -> Result<(), String> {
                if event == "agent-context" {
                    std::thread::sleep(std::time::Duration::from_millis(2000));
                }
                Ok(())
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../.dex/tmp/slow-emit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(Slow),
        )
        .unwrap();
        let bg = std::thread::spawn({
            let core = core.clone();
            move || {
                let t0 = std::time::Instant::now();
                core.agent_context("timur").unwrap();
                t0.elapsed()
            }
        });
        // Arka plan emit'e girsin: ilk snapshot her zaman değişir.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test -p dex-core --lib chat"},
        });
        let t0 = std::time::Instant::now();
        let answer = core.delegation_on_hook("kaan", "PreToolUse", &body, false);
        let decision = t0.elapsed();
        let bg_took = bg.join().unwrap();
        assert!(bg_took >= std::time::Duration::from_millis(2000), "arka plan yavaş emit yapmalıydı: {bg_took:?}");
        assert!(
            decision < std::time::Duration::from_millis(1500),
            "PreToolUse yavaş işin arkasında bekledi: {decision:?}"
        );
        assert!(answer.is_some(), "güvenli komut cevapsız kalmamalı");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #1813: büyük bir transcript kuyruğu işlenirken PreToolUse kararı hızlı döner.
    /// Dosya okuma + JSON çözümleme kilitsiz, parça işlemleri kısa tutulur.
    #[test]
    fn large_transcript_ingest_does_not_block_pre_tool_decision() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../.dex/tmp/slow-ingest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let core = crate::Core::start(
            crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let path = root.join("slow.jsonl");
        let mut bulk = String::new();
        for i in 0..150_000 {
            bulk.push_str(&serde_json::json!({"type":"assistant","timestamp":"2026-10-05T10:00:00Z",
                "message":{"id":format!("m{i}"),"model":"claude-sonnet-5-5",
                "usage":{"input_tokens":100,"output_tokens":10}}}).to_string());
            bulk.push('\n');
        }
        std::fs::write(&path, bulk).unwrap();
        let source: i64 = crate::db::with(&core.db, |c| {
            observe(c, "timur", "slow-session", "claude", Some(path.to_str().unwrap()), "UserPromptSubmit")?;
            c.query_row("SELECT id FROM cost_sources WHERE session='slow-session'", [], |r| r.get(0))
                .map_err(err)
        })
        .unwrap();
        let bg = std::thread::spawn({
            let db = core.db.clone();
            let path = path.clone();
            move || {
                let t0 = std::time::Instant::now();
                ingest_split(&db, source, "claude", &path).unwrap();
                t0.elapsed()
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        let body = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test -p dex-core --lib chat"},
        });
        let t0 = std::time::Instant::now();
        let answer = core.delegation_on_hook("kaan", "PreToolUse", &body, false);
        let decision = t0.elapsed();
        assert!(
            !bg.is_finished(),
            "karar, kuyruk işleme bitmeden dönmeli (yoksa test boş geçer)"
        );
        let bg_took = bg.join().unwrap();
        assert!(bg_took >= std::time::Duration::from_millis(2000), "arka plan kuyruğu işlemeliydi: {bg_took:?}");
        assert!(
            decision < std::time::Duration::from_millis(1500),
            "PreToolUse kuyruk işlemenin arkasında bekledi: {decision:?}"
        );
        assert!(answer.is_some(), "güvenli komut cevapsız kalmamalı");
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn process_exit_closes_only_its_agents_session() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        observe(&c, "kaan", "exit-a", "claude", None, "SessionStart").unwrap();
        observe(&c, "ayla", "exit-b", "claude", None, "SessionStart").unwrap();
        end_agent(&c, "kaan").unwrap();
        assert_eq!(
            c.query_row("SELECT agent_id FROM cost_sessions WHERE ended_at IS NULL", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "ayla"
        );
        assert_eq!(
            c.query_row("SELECT count(*) FROM cost_sources", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn incremental_state_survives_reopen_and_truncation_is_visible() {
        use std::io::Write;
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("cost-incremental-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        observe(&c, "kaan", "inc-test", "claude", Some(path.to_str().unwrap()), "SessionStart").unwrap();
        c.execute("UPDATE cost_sessions SET started_at=0", []).unwrap();
        let row = |id: &str, model: &str, i: u64, o: u64| {
            serde_json::json!({"type":"assistant","timestamp":"2026-10-05T10:00:00Z","message":{"id":id,"model":model,
                "usage":{"input_tokens":i,"output_tokens":o,"cache_read_input_tokens":5}}})
        };
        writeln!(f, "{}", row("m1", "claude-opus-5-5", 95, 10)).unwrap();
        assert_eq!(summary(&c, "default", None, None).unwrap().total.tokens.input, 95);
        writeln!(f, "{}", row("m1", "claude-opus-5-5", 95, 10)).unwrap();
        writeln!(f, "{}", row("m2", "claude-sonnet-5-5", 50, 10)).unwrap();
        let out = summary(&c, "default", None, None).unwrap();
        assert!(out.warnings.is_empty());
        assert_eq!(
            out.total.tokens,
            Tokens {
                input: 145,
                output: 20,
                cache_read: 10,
                cache_write: 0
            }
        );
        assert_eq!(
            c.query_row("SELECT count(DISTINCT model) FROM cost_messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(SCHEMA).unwrap();
        assert_eq!(summary(&c, "default", None, None).unwrap().total.tokens.input, 145);
        c.execute("UPDATE cost_sources SET parser='{}'", []).unwrap();
        let snapshot = context(&c, "kaan").unwrap().0;
        assert_eq!(snapshot.tokens, 55);
        assert_eq!(snapshot.window, Some(1_000_000));
        assert_eq!(summary(&c, "default", None, None).unwrap().total.tokens.input, 145);
        observe(&c, "kaan", "inc-fresh", "claude", None, "SessionStart").unwrap();
        let fresh = context(&c, "kaan").unwrap().0;
        assert_eq!(fresh.tokens, 0);
        assert_eq!(fresh.session_id.as_deref(), Some("inc-fresh"));
        f.set_len(0).unwrap();
        let truncated = summary(&c, "default", None, None).unwrap();
        assert_eq!(truncated.total.tokens.input, 145);
        assert!(truncated.warnings[0].contains("küçüldü"));
        drop(f);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn report_tail_stays_with_finished_job_until_next_prompt() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(41,'timur','kaan','default','x','running',0,0)",[]).unwrap();
        observe(&c, "kaan", "jobs", "claude", None, "SessionStart").unwrap();
        c.execute("UPDATE delegations SET status='done' WHERE id=41", []).unwrap();
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(42,'timur','kaan','default','x','queued',0,0)",[]).unwrap();
        observe(&c, "kaan", "jobs", "claude", None, "PostToolUse").unwrap();
        let active = || {
            c.query_row("SELECT delegation_id FROM cost_sessions WHERE ended_at IS NULL", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        assert_eq!(active(), 41);
        observe(&c, "kaan", "jobs", "claude", None, "UserPromptSubmit").unwrap();
        assert_eq!(active(), 42);
        c.execute("UPDATE agents SET active_provider='glm',active_model='GLM-5.3' WHERE id='kaan'", [])
            .unwrap();
        observe(&c, "kaan", "jobs", "claude", None, "SessionStart").unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM cost_sessions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            c.query_row("SELECT provider FROM cost_sessions WHERE ended_at IS NULL", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "glm"
        );
        observe(&c, "kaan", "jobs", "claude", None, "SessionEnd").unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM cost_sessions WHERE ended_at IS NULL", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn usage_timestamps_support_seconds_and_microseconds() {
        let parse = |s| crate::chat::parse_ts(&Value::String(s));
        let base = parse("2026-10-05T10:00:00.000Z".into()).unwrap();
        assert_eq!(parse("2026-10-05T10:00:00Z".into()), Some(base));
        assert_eq!(parse("2026-10-05T10:00:00.123456Z".into()), Some(base + 123));
        assert_eq!(parse("2026-10-05T10:00:00.1Z".into()), Some(base + 100));
    }

    #[test]
    fn pricing_and_delegation_totals_are_provider_scoped() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        observe(&c, "kaan", "priced", "claude", None, "SessionStart").unwrap();
        c.execute("UPDATE cost_sessions SET delegation_id=42", []).unwrap();
        c.execute(
            "INSERT INTO cost_messages SELECT source_id,'m',id,'shared-model',100,1000000,2000000,3000000,4000000 FROM cost_sessions",
            [],
        )
        .unwrap();
        let rates = Rates {
            input: 1.0,
            output: 2.0,
            cache_read: 0.5,
            cache_write: 1.5,
        };
        set_prices(
            &c,
            vec![Price {
                provider: "glm".into(),
                model: "shared-model".into(),
                rates: Some(rates),
            }],
        )
        .unwrap();
        assert_eq!(summary(&c, "default", None, Some(42)).unwrap().total.usd, None);
        set_prices(
            &c,
            vec![Price {
                provider: "anthropic".into(),
                model: "shared-model".into(),
                rates: Some(rates),
            }],
        )
        .unwrap();
        let out = summary(&c, "default", None, Some(42)).unwrap();
        assert_eq!(out.total.usd, Some(12.5));
        assert_eq!(out.agents["kaan"].usd, Some(12.5));
        assert_eq!(out.delegations[&42].usd, Some(12.5));
        assert_eq!(summary(&c, "default", Some(101), None).unwrap().total.tokens.input, 0);
        assert!(set_prices(
            &c,
            vec![Price {
                provider: "anthropic".into(),
                model: "bad".into(),
                rates: Some(Rates { input: -1.0, ..rates })
            }]
        )
        .is_err());
    }

    /// Old non-Claude sources are not re-read (no warning), yet their stored messages still count.
    #[test]
    fn legacy_engine_sources_skip_ingest_but_keep_totals() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        observe(&c, "ayla", "legacy-codex-session", "codex", None, "SessionStart").unwrap();
        c.execute(
            "INSERT INTO cost_messages SELECT source_id,'m',id,'gpt-5.5',100,7,3,0,0 FROM cost_sessions",
            [],
        )
        .unwrap();
        let messages = |c: &Connection| -> i64 { c.query_row("SELECT count(*) FROM cost_messages", [], |r| r.get(0)).unwrap() };
        let before = messages(&c);
        let out = summary(&c, "default", None, None).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.total.tokens.input, 7);
        assert_eq!(out.agents["ayla"].tokens.output, 3);
        assert_eq!(messages(&c), before);
        let (ctx, _) = context(&c, "ayla").unwrap();
        assert_eq!(ctx.session_id, None);
        assert_eq!(messages(&c), before);
    }

    #[test]
    fn migration_and_incremental_ingestion_preserve_data_and_deduplicate() {
        use std::io::Write;
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        let before: i64 = c.query_row("SELECT count(*) FROM agents", [], |r| r.get(0)).unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(CONTEXT_SCHEMA).unwrap();
        c.execute_batch(CONTEXT_SCHEMA).unwrap();
        assert_eq!(
            before,
            c.query_row("SELECT count(*) FROM agents", [], |r| r.get::<_, i64>(0)).unwrap()
        );
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("cost-{}.jsonl", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        observe(&c, "kaan", "s1", "claude", Some(path.to_str().unwrap()), "SessionStart").unwrap();
        c.execute("UPDATE cost_sessions SET started_at=0", []).unwrap();
        let line = serde_json::json!({"type":"assistant","timestamp":"2026-10-05T10:00:00.000Z",
            "message":{"id":"m1","model":"claude-test","usage":{"input_tokens":10,"output_tokens":5}}})
        .to_string();
        writeln!(f, "{line}").unwrap();
        write!(f, "{}", &line[..20]).unwrap();
        let first = summary(&c, "default", None, None).unwrap();
        assert_eq!(first.total.tokens.input, 10);
        assert_eq!(first.total.usd, None);
        assert_eq!(first.agents["kaan"].tokens.output, 5);
        writeln!(f, "{}", &line[20..]).unwrap();
        let second = summary(&c, "default", None, None).unwrap();
        assert_eq!(second.total.tokens.input, 10, "duplicate message and partial append count once");
        assert_eq!(summary(&c, "other", None, None).unwrap().total.tokens.input, 0);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn claude_usage_keeps_message_model_and_cache_categories() {
        let mut state = ParserState::default();
        let v = serde_json::json!({"type":"assistant","timestamp":"2026-10-05T10:00:00.000Z",
            "message":{"id":"msg-1","model":"GLM-5.3","usage":{
                "input_tokens":10,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":40}}});
        let row = parse_usage(&v, "claude", &mut state).unwrap();
        assert_eq!(row.model, "GLM-5.3");
        assert_eq!(row.key, "msg-1");
        assert_eq!(
            row.tokens,
            Tokens {
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40
            }
        );
    }
}
