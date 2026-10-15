//! Plan #114: what the patron controls in the agents' prompts, beside the role catalog and
//! [`crate::rules`].
//! - Per team, two leader-only appends (Takım notu): `system_append` joins the leader's system
//!   prompt when its session starts; `message_append` joins every patron prompt the leader gets
//!   (UserPromptSubmit `additionalContext`), never a `[Dex-…]` paste. Only the patron writes them
//!   (RPC); the leader reads them with `team_note_get`.
//! - Global overrides of Dex's built-in prompt parts ([`parts`]): the rules template and each role
//!   text are editable; dynamic blocks are listed read-only with their template. Policy (permit,
//!   hook denials) is code: editing a text never lifts a rule.
//! - Applied state: a leader spawn records the fingerprint of the patron inputs its system prompt
//!   was built from ([`fingerprint`]); `pendingRestart` = the recorded print differs from now.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

// 42: additive, idempotent (IF NOT EXISTS in a versioned migration). Before commit: ROLLBACK.
// After commit: keep the tables; revert only the application (nothing else reads them). Full
// undo (drops the patron's texts): DROP TABLE leader_prompt_applied; DROP TABLE prompt_overrides;
// DROP TABLE leader_prompts;
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS leader_prompts (
    team_id        TEXT PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
    system_append  TEXT NOT NULL DEFAULT '',
    message_append TEXT NOT NULL DEFAULT '',
    updated_at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS prompt_overrides (
    part_id    TEXT PRIMARY KEY,
    body       TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS leader_prompt_applied (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    applied_at  INTEGER NOT NULL
);";

/// The system prompt goes on the CLI's command line (Windows: 32 767 UTF-16 units in all); byte
/// limits are the safe side of that count. Rules ~7.4 KB, role ~1.2 KB, stack ≤ 4 KB today.
const APPEND_MAX: usize = 4 * 1024;
const RULES_MAX: usize = 12 * 1024;
const ROLE_MAX: usize = 4 * 1024;

pub const SYSTEM_HEAD: &str = "## Patronun ek talimatı (takım notu)";
pub const MESSAGE_HEAD: &str = "## Patronun mesaj eki (takım notu)";

pub const RULES_PART: &str = "dex-rules";
const ROLE_PREFIX: &str = "role:";

pub const POLICY_NOTE: &str = "İzin ve politika kuralları (permit/hook reddi: AskUserQuestion, tam test paketi, lider e2e, izinli yollar, inceleyici kabuk/hafıza sınırı, rapor sözleşmesi) koddadır; istem metnini değiştirmek kuralı kaldırmaz, yalnız ajana anlatılanı değiştirir.";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

// ---- leader appends ----

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LeaderPrompt {
    pub team_id: String,
    pub system_append: String,
    pub message_append: String,
    /// 0 = never written.
    pub updated_at: i64,
    /// The leader's last session start used other patron inputs (system append, an override that
    /// reaches it) than the current ones: they apply at its next session start. The message
    /// append never sets it.
    pub pending_restart: bool,
    /// The leader's last session start with Dex's prompt; 0 = none recorded.
    pub applied_at: i64,
}

fn check_team(conn: &Connection, team: &str) -> Result<(), String> {
    conn.query_row("SELECT 1 FROM teams WHERE id = ?1", [team], |_| Ok(()))
        .optional()
        .map_err(e)?
        .ok_or_else(|| format!("takım yok: {team}"))
}

pub fn get(conn: &Connection, team: &str) -> Result<LeaderPrompt, String> {
    check_team(conn, team)?;
    let (system_append, message_append, updated_at) = conn
        .query_row("SELECT system_append, message_append, updated_at FROM leader_prompts WHERE team_id = ?1", [team], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .map_err(e)?
        .unwrap_or_default();
    let (pending_restart, applied_at) = applied_state(conn, team)?;
    Ok(LeaderPrompt { team_id: team.into(), system_append, message_append, updated_at, pending_restart, applied_at })
}

/// `None` leaves that field as it is.
pub fn set(conn: &Connection, team: &str, system: Option<&str>, message: Option<&str>) -> Result<LeaderPrompt, String> {
    check_team(conn, team)?;
    for (what, text) in [("sistem eki", system), ("mesaj eki", message)] {
        if text.is_some_and(|t| t.len() > APPEND_MAX) {
            return Err(format!("{what} çok uzun (en çok {} KB)", APPEND_MAX / 1024));
        }
    }
    if system.is_some() || message.is_some() {
        conn.execute(
            "INSERT INTO leader_prompts (team_id, system_append, message_append, updated_at)
             VALUES (?1, COALESCE(?2, ''), COALESCE(?3, ''), ?4)
             ON CONFLICT(team_id) DO UPDATE SET system_append = COALESCE(?2, system_append),
                 message_append = COALESCE(?3, message_append), updated_at = ?4",
            params![team, system, message, now()],
        )
        .map_err(e)?;
    }
    get(conn, team)
}

fn leader_append(conn: &Connection, agent: &str, column: &str) -> Option<String> {
    let sql = format!(
        "SELECT p.{column} FROM agents a JOIN leader_prompts p ON p.team_id = a.team_id WHERE a.id = ?1 AND a.kind = 'leader'"
    );
    conn.query_row(&sql, [agent], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// The leader's system append (trimmed); None for a non-leader or an empty field.
pub(crate) fn system_append_for(conn: &Connection, agent: &str) -> Option<String> {
    leader_append(conn, agent, "system_append")
}

/// The patron's per-message append for a prompt the leader got; `[Dex-…]` turns get none.
pub(crate) fn message_context(conn: &Connection, agent: &str, prompt: &str) -> Option<String> {
    if crate::dexmsg::is_dex_turn(prompt) {
        return None;
    }
    leader_append(conn, agent, "message_append").map(|t| format!("{MESSAGE_HEAD}\n{t}"))
}

/// Adds [`message_context`] to the UserPromptSubmit answer `out` (`additionalContext`, after
/// Dex's own context); a blocked prompt stays as it is.
pub(crate) fn with_message_append(db: &crate::Db, agent: &str, body: &Value, out: Option<String>) -> Option<String> {
    let prompt = body.get("prompt").and_then(Value::as_str).unwrap_or("");
    let Ok(Some(extra)) = crate::db::with(db, |c| Ok(message_context(c, agent, prompt))) else { return out };
    let mut v: Value = match &out {
        Some(s) => match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => return out,
        },
        None => json!({}),
    };
    if v["decision"] == "block" {
        return out;
    }
    let ctx = v
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(Value::as_str)
        .map(|c| format!("{c}\n{extra}"))
        .unwrap_or(extra);
    v["hookSpecificOutput"]["hookEventName"] = json!("UserPromptSubmit");
    v["hookSpecificOutput"]["additionalContext"] = json!(ctx);
    Some(v.to_string())
}

/// What a leader's system prompt takes from the patron: its team's system append and the overrides
/// that reach it (rules template, its role's text). None for a non-leader.
pub(crate) fn fingerprint(conn: &Connection, agent: &str) -> Option<String> {
    let (team, role): (String, Option<String>) = conn
        .query_row("SELECT team_id, role_slug FROM agents WHERE id = ?1 AND kind = 'leader'", [agent], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .ok()
        .flatten()?;
    Some(fingerprint_of(conn, &team, role.as_deref()))
}

fn fingerprint_of(conn: &Connection, team: &str, role: Option<&str>) -> String {
    let system: String = conn
        .query_row("SELECT system_append FROM leader_prompts WHERE team_id = ?1", [team], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();
    let rules = override_body(conn, RULES_PART);
    let role = role.and_then(|r| override_body(conn, &format!("{ROLE_PREFIX}{r}")));
    let inputs = json!([system.trim(), rules, role]).to_string();
    format!("{:x}", Sha256::digest(inputs.as_bytes()))
}

/// A leader session started with a prompt built from `print`; the leader's team, for the event.
pub(crate) fn record_applied(conn: &Connection, agent: &str, print: &str) -> Result<String, String> {
    conn.execute(
        "INSERT INTO leader_prompt_applied (agent_id, fingerprint, applied_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(agent_id) DO UPDATE SET fingerprint = excluded.fingerprint, applied_at = excluded.applied_at",
        params![agent, print, now()],
    )
    .map_err(e)?;
    conn.query_row("SELECT team_id FROM agents WHERE id = ?1", [agent], |r| r.get(0)).map_err(e)
}

/// Any of the team's leaders started from other inputs than now (a leader never recorded since
/// this feature has nothing to restart for); the latest start.
fn applied_state(conn: &Connection, team: &str) -> Result<(bool, i64), String> {
    let mut st = conn
        .prepare(
            "SELECT a.role_slug, p.fingerprint, p.applied_at FROM agents a
             JOIN leader_prompt_applied p ON p.agent_id = a.id WHERE a.team_id = ?1 AND a.kind = 'leader'",
        )
        .map_err(e)?;
    let rows = st
        .query_map([team], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))
        .map_err(e)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(e)?;
    let pending = rows.iter().any(|(role, print, _)| *print != fingerprint_of(conn, team, role.as_deref()));
    Ok((pending, rows.iter().map(|r| r.2).max().unwrap_or(0)))
}

/// Teams with a leader: an override change may flip their `pendingRestart`.
pub(crate) fn leader_teams(conn: &Connection) -> Result<Vec<String>, String> {
    let mut st = conn.prepare("SELECT DISTINCT team_id FROM agents WHERE kind = 'leader' ORDER BY team_id").map_err(e)?;
    let rows = st.query_map([], |r| r.get(0)).map_err(e)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(e)
}

// ---- Dex's built-in prompt parts ----

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromptPart {
    pub id: String,
    pub title: String,
    /// Who gets it: `leader`, `worker`, `all` (tagged lines choose inside) or `role` (+ `role`).
    pub audience: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// `system` = the session's system prompt (`--append-system-prompt`); `session-start` = the
    /// SessionStart hook context. Both need a new session to change.
    pub channel: &'static str,
    pub editable: bool,
    /// Dex's text; for a read-only dynamic part its template (`<…>` is filled at the start).
    pub default_text: String,
    /// What is in effect: the override, else the default.
    pub text: String,
    pub overridden: bool,
    /// The override's write time; 0 = none.
    pub updated_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Parts {
    pub parts: Vec<PromptPart>,
    pub policy_note: &'static str,
}

pub(crate) fn override_body(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT body FROM prompt_overrides WHERE part_id = ?1", [id], |r| r.get(0)).optional().ok().flatten()
}

fn part(id: &str, title: impl Into<String>, audience: &'static str, channel: &'static str, default_text: String, note: Option<&str>) -> PromptPart {
    PromptPart {
        id: id.into(),
        title: title.into(),
        audience,
        role: None,
        channel,
        editable: false,
        text: default_text.clone(),
        default_text,
        overridden: false,
        updated_at: 0,
        note: note.map(String::from),
    }
}

fn with_override(conn: &Connection, mut p: PromptPart) -> Result<PromptPart, String> {
    p.editable = true;
    let row: Option<(String, i64)> = conn
        .query_row("SELECT body, updated_at FROM prompt_overrides WHERE part_id = ?1", [&p.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(e)?;
    if let Some((body, at)) = row {
        p.text = body;
        p.overridden = true;
        p.updated_at = at;
    }
    Ok(p)
}

/// Every part in prompt order: editable ones first (rules, roles), then the read-only blocks.
pub fn parts(conn: &Connection) -> Result<Parts, String> {
    let mut out = vec![with_override(
        conn,
        part(RULES_PART, "Dex çalışma kuralları", "all", "system", crate::rules::DEX_RULES.into(), Some(
            "Etiketli satırlar: `- [Lider] ` / `- [Worker] ` yalnız o türe, `- [QA] ` yalnız QA worker'a, `- [İnceleyici] ` yalnız kod inceleyiciye, `- [-İnceleyici] ` inceleyici dışındakilere gider. `{shared}`, `{own}` (hafıza klasörleri) ve `{roles}` (rol kataloğu, lider) açılışta doldurulur; `## Hafıza` bölümü hafızasız ajanda düşer. Eğitim profili, hafif test düzeyi ve hafıza kapsam satırı bu metni başlık/satır başına göre dönüştürür; o satırları silersen dönüşüm atlanır. Asistan kendi kurallarını alır.",
        )),
    )?];
    let mut st = conn.prepare("SELECT slug, name, system_prompt FROM roles ORDER BY sort, slug").map_err(e)?;
    let roles = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(e)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(e)?;
    for (slug, name, text) in roles {
        let mut p = part(&format!("{ROLE_PREFIX}{slug}"), format!("Rol: {name}"), "role", "system", text, Some("İstemin başında; bu roldeki her ajana (lider ya da worker) gider."));
        p.role = Some(slug);
        out.push(with_override(conn, p)?);
    }
    let (light, light_leader) = crate::rules::LIGHT_TEST;
    let readonly = [
        part("stack-profile", "Proje yığını", "all", "system", "## Proje yığını\n<repo kökündeki .dex/stack.md, en çok 4 KB>".into(), Some("Repo dosyasıdır; projede düzenlenir. Rol metninden sonra, kurallardan önce gelir.")),
        part("test-light", "Hafif test düzeyi", "all", "system", format!("{light}{light_leader}"), Some("Takımın test düzeyi hafifken `## Test bütçesi` bölümünün yerine geçer; son satır yalnız lidere.")),
        part("education", "Eğitim profili", "all", "system", "<eğitim takımında `## Test bütçesi` ve geliştirme satırları düşer; çıktı sözleşmesi, iş verme ve teslim satırları ders akışına göre yazılır>".into(), Some("Kod içinde Dex kurallarından türetilir.")),
        part("memory-scope", "Hafıza kapsam satırı", "all", "system", format!("- Scope: <bağlama göre: repo kök nedeni → proje shared / isteyen projenin hafızasına yazma / proje seçili değil> {}", crate::rules::SCOPE_TAIL), Some("Yalnız bağlam hafızayı projeye böldüğünde `## Hafıza` altına girer.")),
        part("gpu", "GPU kaynağı", "leader", "system", crate::gpu::leader_text("<türler>"), Some("Yalnız GPU kaynağı bağlıyken.")),
        part("stack-note", "Proje yığını notu", "leader", "system", "## Proje yığını notu\n<.dex/stack.md yoksa tespit edilen teknoloji ve taslak çağrısı; kesildiyse boyut uyarısı>".into(), None),
        part("test-plan-missing", "Test bütçesi planı uyarısı", "leader", "system", "Test bütçesi planı yok: <çalıştırıcılar> (<proje>) — patrona kural planı önermeyi düşün.".into(), Some("Repoda planlanmamış tam-paket çalıştırıcı varsa.")),
        part("repo-map", "Proje haritası satırı", "all", "system", crate::repomap::prompt_text("<repo kökü>"), Some("Yalnız .dex/map.md varken.")),
        part("job-context", "İş/proje bağlamı", "all", "system", "## İş/proje bağlamı\nTakım: <takım>\nProje: <id> (<ad>)\nİsteyen takım: <takım>\nİş: <id>\nRepo kökü: <yol>\nHedef dal: <dal>\nRapor: <yol>".into(), Some("Oturumun bağlamından; olmayan satır yazılmaz.")),
        part("leader-append", "Patronun ek talimatı", "leader", "system", format!("{SYSTEM_HEAD}\n<Takım notu → sistem eki>"), Some("Takım notunda düzenlenir; istemin en sonunda.")),
        part("assistant", "Asistan kuralları", "role", "system", crate::rules::assistant_rules(Some((Path::new("<takım ortak>"), Path::new("<sana özel>")))), Some("Asistan rolü Dex kuralları yerine bunu alır.")),
        part("roster", "Takım kadrosu", "leader", "session-start", "# Takım kadrosu\n- <ad> (<id>) — <rol>/<unvan> — <motor>/<model> — açık|kapalı\n# Takım projeleri\n- <id> — <ad> — aktif|bağlı değil|arşiv — <dal>".into(), Some("Oturum anındaki veritabanından.")),
        part("memory-index", "Hafıza indeksi", "all", "session-start", "# Dex hafıza indeksi\nAyrıntı gerekirse dosyayı oku; kurallar sistem isteminde.\n## Takım ortak — <klasör>\n- <slug> [<tür>] — <açıklama> — <yol>\n## Sana özel — <klasör>\n…".into(), Some("Hafıza klasörlerinden, en çok 8 KB.")),
        part("pending", "Bekleyen sonuç ve sorular", "leader", "session-start", "<lider kapalıyken biten devretme sonuçları ve worker soruları>".into(), None),
    ];
    for mut p in readonly {
        if p.id == "assistant" {
            p.role = Some("assistant".into());
        }
        out.push(p);
    }
    Ok(Parts { parts: out, policy_note: POLICY_NOTE })
}

fn editable_default(conn: &Connection, id: &str) -> Result<(String, usize), String> {
    if id == RULES_PART {
        return Ok((crate::rules::DEX_RULES.into(), RULES_MAX));
    }
    if let Some(slug) = id.strip_prefix(ROLE_PREFIX) {
        let text: Option<String> = conn.query_row("SELECT system_prompt FROM roles WHERE slug = ?1", [slug], |r| r.get(0)).optional().map_err(e)?;
        return text.map(|t| (t, ROLE_MAX)).ok_or_else(|| format!("parça yok: {id}"));
    }
    if parts(conn)?.parts.iter().any(|p| p.id == id) {
        return Err(format!("salt okunur parça: {id}"));
    }
    Err(format!("parça yok: {id}"))
}

fn find(conn: &Connection, id: &str) -> Result<PromptPart, String> {
    parts(conn)?.parts.into_iter().find(|p| p.id == id).ok_or_else(|| format!("parça yok: {id}"))
}

/// The patron's text for an editable part. The default's own text removes the override.
pub fn part_set(conn: &Connection, id: &str, text: &str) -> Result<PromptPart, String> {
    let (default, max) = editable_default(conn, id)?;
    if text.trim().is_empty() {
        return Err("boş metin; varsayılana dönmek için prompt_part_reset".into());
    }
    if text.len() > max {
        return Err(format!("metin çok uzun (en çok {} KB)", max / 1024));
    }
    if text == default {
        conn.execute("DELETE FROM prompt_overrides WHERE part_id = ?1", [id]).map_err(e)?;
    } else {
        conn.execute(
            "INSERT INTO prompt_overrides (part_id, body, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(part_id) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
            params![id, text, now()],
        )
        .map_err(e)?;
    }
    find(conn, id)
}

/// "Varsayılana dön": the override goes, Dex's text is in effect again.
pub fn part_reset(conn: &Connection, id: &str) -> Result<PromptPart, String> {
    editable_default(conn, id)?;
    conn.execute("DELETE FROM prompt_overrides WHERE part_id = ?1", [id]).map_err(e)?;
    find(conn, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        crate::db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn appends_roundtrip_partially_and_refuse_unknown_team_or_oversize() {
        let c = db();
        let empty = get(&c, "default").unwrap();
        assert_eq!((empty.system_append.as_str(), empty.message_append.as_str(), empty.updated_at, empty.pending_restart), ("", "", 0, false));
        let a = set(&c, "default", Some("Türkçe yaz."), None).unwrap();
        assert!(a.updated_at > 0 && a.message_append.is_empty());
        let b = set(&c, "default", None, Some("Kısa tut.")).unwrap();
        assert_eq!((b.system_append.as_str(), b.message_append.as_str()), ("Türkçe yaz.", "Kısa tut."), "None alanı korur");
        assert!(get(&c, "yok-0000").unwrap_err().contains("takım yok"));
        assert!(set(&c, "yok-0000", Some("x"), None).is_err());
        assert!(set(&c, "default", Some(&"x".repeat(APPEND_MAX + 1)), None).unwrap_err().contains("uzun"));
        assert_eq!(get(&c, "default").unwrap().system_append, "Türkçe yaz.", "reddedilen yazım değiştirmez");
        c.execute("INSERT INTO teams (id, name, kind, created_at) VALUES ('t-0001', 'T', 'service', 0)", []).unwrap();
        set(&c, "t-0001", Some("gider"), Some("gider")).unwrap();
        c.execute("DELETE FROM teams WHERE id = 't-0001'", []).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM leader_prompts", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "takımla gider");
    }

    #[test]
    fn appends_reach_only_the_leader_and_patron_turns() {
        let c = db();
        assert_eq!(system_append_for(&c, "timur"), None);
        set(&c, "default", Some("  \n "), Some("")).unwrap();
        assert_eq!(system_append_for(&c, "timur"), None, "boşluk eklenmez");
        assert_eq!(message_context(&c, "timur", "merhaba"), None);
        set(&c, "default", Some(" Sistem eki \n"), Some("Mesaj eki")).unwrap();
        assert_eq!(system_append_for(&c, "timur").as_deref(), Some("Sistem eki"));
        assert_eq!(system_append_for(&c, "kaan"), None, "worker almaz");
        assert_eq!(message_context(&c, "timur", "merhaba").unwrap(), format!("{MESSAGE_HEAD}\nMesaj eki"));
        assert_eq!(message_context(&c, "kaan", "merhaba"), None);
        for dex in ["[Dex-Devretme] Kaan #1 · bitti — x", "[Dex] yeni iş #2", "  [Dex-Soru] Kaan #3 · soru"] {
            assert_eq!(message_context(&c, "timur", dex), None, "{dex}");
        }
        assert!(message_context(&c, "timur", "[Dexter] patron yazdı").is_some());
    }

    #[test]
    fn pending_restart_follows_system_inputs_not_the_message_append() {
        let c = db();
        assert_eq!(fingerprint(&c, "kaan"), None, "yalnız lider");
        assert!(!get(&c, "default").unwrap().pending_restart, "kayıt yokken bekleyen yok");
        set(&c, "default", Some("eski"), None).unwrap();
        assert!(!get(&c, "default").unwrap().pending_restart, "lider hiç başlamadı: sonraki açılışta uygulanır");
        let start = |c: &Connection| record_applied(c, "timur", &fingerprint(c, "timur").unwrap()).unwrap();
        assert_eq!(start(&c), "default");
        let applied = get(&c, "default").unwrap();
        assert!(!applied.pending_restart && applied.applied_at > 0);
        set(&c, "default", None, Some("anında")).unwrap();
        assert!(!get(&c, "default").unwrap().pending_restart, "mesaj eki sarı kuşak açmaz");
        set(&c, "default", Some("yeni"), None).unwrap();
        assert!(get(&c, "default").unwrap().pending_restart);
        set(&c, "default", Some(" eski\n"), None).unwrap();
        assert!(!get(&c, "default").unwrap().pending_restart, "uygulanana dönmek kuşağı kapatır; kenar boşluğu sayılmaz");
        part_set(&c, "role:frontend", "worker rolü").unwrap();
        assert!(!get(&c, "default").unwrap().pending_restart, "liderin rolü değil");
        part_set(&c, "role:lead", "lider rolü").unwrap();
        assert!(get(&c, "default").unwrap().pending_restart);
        part_reset(&c, "role:lead").unwrap();
        part_set(&c, RULES_PART, "# kurallar").unwrap();
        assert!(get(&c, "default").unwrap().pending_restart);
        start(&c);
        assert!(!get(&c, "default").unwrap().pending_restart, "yeniden başlayınca kapanır");
    }

    #[test]
    fn parts_list_editable_overrides_and_reset() {
        let c = db();
        let all = parts(&c).unwrap();
        assert_eq!(all.policy_note, POLICY_NOTE);
        let rules = &all.parts[0];
        assert_eq!((rules.id.as_str(), rules.editable, rules.overridden, rules.text.as_str()), (RULES_PART, true, false, crate::rules::DEX_RULES));
        let lead = all.parts.iter().find(|p| p.id == "role:lead").unwrap();
        assert!(lead.editable && lead.role.as_deref() == Some("lead") && !lead.default_text.is_empty());
        for id in ["gpu", "roster", "memory-index", "stack-profile", "job-context", "leader-append", "assistant"] {
            let p = all.parts.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("{id}"));
            assert!(!p.editable && !p.default_text.is_empty(), "{id}");
            assert!(part_set(&c, id, "x").unwrap_err().contains("salt okunur"), "{id}");
        }
        assert!(all.parts.iter().find(|p| p.id == "gpu").unwrap().default_text.contains("<türler>"));
        assert!(part_set(&c, "yok", "x").unwrap_err().contains("parça yok"));
        assert!(part_set(&c, "role:yok", "x").unwrap_err().contains("parça yok"));
        assert!(part_set(&c, RULES_PART, "  ").unwrap_err().contains("boş"));
        assert!(part_set(&c, "role:lead", &"x".repeat(ROLE_MAX + 1)).unwrap_err().contains("uzun"));
        let p = part_set(&c, "role:lead", "Rolün: patronun lideri.").unwrap();
        assert!(p.overridden && p.updated_at > 0 && p.text == "Rolün: patronun lideri." && p.default_text == lead.default_text);
        let p = part_set(&c, "role:lead", &lead.default_text).unwrap();
        assert!(!p.overridden, "varsayılan metin override tutmaz");
        part_set(&c, "role:lead", "x").unwrap();
        let p = part_reset(&c, "role:lead").unwrap();
        assert!(!p.overridden && p.text == lead.default_text && p.updated_at == 0);
        assert!(part_reset(&c, "gpu").unwrap_err().contains("salt okunur"));
    }

    #[test]
    fn overrides_reach_the_built_prompt_and_the_append_only_the_leader() {
        let c = db();
        part_set(&c, RULES_PART, "# Özel kurallar\n- [Lider] yalnız lider satırı\n- [Worker] yalnız worker satırı\n- herkese").unwrap();
        part_set(&c, "role:backend", "Rolün: özel arka uç.").unwrap();
        set(&c, "default", Some("Lider eki"), Some("mesaj")).unwrap();
        let leader = crate::rules::system_prompt(&c, "timur", None);
        let worker = crate::rules::system_prompt(&c, "ayla", None);
        assert!(leader.contains("# Özel kurallar") && leader.contains("- yalnız lider satırı") && !leader.contains("yalnız worker"), "{leader}");
        assert!(!leader.contains("Dex çalışma kuralları"), "şablon yerine geçer");
        assert!(leader.ends_with(&format!("{SYSTEM_HEAD}\nLider eki")), "{leader}");
        assert!(worker.starts_with("Rolün: özel arka uç.") && worker.contains("- yalnız worker satırı"), "{worker}");
        assert!(!worker.contains("Lider eki") && !worker.contains(SYSTEM_HEAD) && !worker.contains("mesaj"));
        part_reset(&c, RULES_PART).unwrap();
        assert!(crate::rules::system_prompt(&c, "ayla", None).contains("# Dex çalışma kuralları"));
    }
}
