//! Görev #286: a worker's branch lands in the team branch only after a team member who did not
//! write it reviewed it. Deterministic, in the core — not left to the leader's memory.
//!
//! - A job that reports with commits on its own branch gets a review job: the team's code-review
//!   role pool, never the author. The review has no worktree; it reads the
//!   diff from the job's main checkout (#888: the review job carries the reviewed job's project
//!   binding). #287: no code-review member in the team → no gate at all, the merge
//!   lands as before #286 (decided at merge time). Only member is the author → the merge waits for
//!   the patron (`forceReview`).
//! - The reviewer's report opens with `karar: onay` or `karar: düzelt`; `report` refuses anything
//!   else. The verdict is bound to the commit it reviewed: a newer tip is not covered.
//! - `delegation_merge` lands a branch only on `onay` for its tip. A tip with no review and none
//!   running gets one opened by the merge attempt.
//! - Fix loop: `delegate` with `fixOf` starts the fix job's branch on the reviewed job's tip
//!   (`delegation_fixes`); once the fix lands, the older branch is in the team branch and goes.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::delegate::{self, Delegation};
use crate::worktree::Wt;

/// The catalog role that reviews.
pub const ROLE: &str = "code-review";

/// Criteria for a team folder without its own REVIEW.md (Dex's own, compiled in).
const CRITERIA: &str = include_str!("../../../REVIEW.md");

#[derive(Debug, Clone, PartialEq)]
pub struct Review {
    pub review_id: i64,
    pub target_id: i64,
    pub sha: String,
    /// `onay` | `düzelt`; `None` while the review runs (or ended without a report).
    pub verdict: Option<String>,
    /// The `önemli` findings, as the reviewer wrote them.
    pub findings: Option<String>,
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

const COLS: &str = "review_id, target_id, sha, verdict, findings";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Review> {
    Ok(Review { review_id: r.get(0)?, target_id: r.get(1)?, sha: r.get(2)?, verdict: r.get(3)?, findings: r.get(4)? })
}

pub fn get(conn: &Connection, review_id: i64) -> Result<Option<Review>, String> {
    conn.query_row(&format!("SELECT {COLS} FROM delegation_reviews WHERE review_id = ?1"), [review_id], row).optional().map_err(e)
}

/// The newest review of `target`'s commit `sha`.
fn of_sha(conn: &Connection, target: i64, sha: &str) -> Result<Option<Review>, String> {
    conn.query_row(
        &format!("SELECT {COLS} FROM delegation_reviews WHERE target_id = ?1 AND sha = ?2 ORDER BY review_id DESC LIMIT 1"),
        params![target, sha],
        row,
    )
    .optional()
    .map_err(e)
}

/// The newest review of `target`, any commit.
fn latest(conn: &Connection, target: i64) -> Result<Option<Review>, String> {
    conn.query_row(&format!("SELECT {COLS} FROM delegation_reviews WHERE target_id = ?1 ORDER BY review_id DESC LIMIT 1"), [target], row)
        .optional()
        .map_err(e)
}

/// A review of `target` whose job is still open.
fn running(conn: &Connection, target: i64) -> Result<Option<i64>, String> {
    conn.query_row(
        "SELECT r.review_id FROM delegation_reviews r JOIN delegations d ON d.id = r.review_id
         WHERE r.target_id = ?1 AND r.verdict IS NULL AND d.status IN ('queued','running') ORDER BY r.review_id DESC LIMIT 1",
        [target],
        |r| r.get(0),
    )
    .optional()
    .map_err(e)
}

pub fn is_review(conn: &Connection, delegation: i64) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS (SELECT 1 FROM delegation_reviews WHERE review_id = ?1)", [delegation], |r| r.get(0)).map_err(e)
}

/// The job whose branch `delegation`'s branch starts on (`delegate` with `fixOf`).
pub fn fix_of(conn: &Connection, delegation: i64) -> Result<Option<i64>, String> {
    conn.query_row("SELECT fix_of FROM delegation_fixes WHERE delegation_id = ?1", [delegation], |r| r.get(0)).optional().map_err(e)
}

/// `fixOf` of a new job: a job of the caller's team that has an unmerged branch.
pub fn check_fix_of(conn: &Connection, team: &str, fix: i64) -> Result<(), String> {
    let d = delegate::get(conn, fix)?;
    if d.team_id != team || d.service_team_id.is_some() {
        return Err(format!("fixOf #{fix} senin takımının işi değil"));
    }
    match crate::worktree::get(conn, fix)? {
        Some(w) if w.merged_at.is_none() => Ok(()),
        Some(_) => Err(format!("fixOf #{fix} zaten birleştirildi; düzeltme takım dalının üstünde açılır, fixOf gerekmez")),
        None => Err(format!("fixOf #{fix} kendi dalında çalışmadı; düzeltme dalı açılamaz")),
    }
}

pub fn set_fix_of(conn: &Connection, delegation: i64, fix: i64) -> Result<(), String> {
    conn.execute("INSERT OR REPLACE INTO delegation_fixes (delegation_id, fix_of) VALUES (?1, ?2)", params![delegation, fix]).map_err(e)?;
    Ok(())
}

/// #287: the team has a code-review worker — the gate is on.
pub fn gate_on(conn: &Connection, team: &str) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS (SELECT 1 FROM agents WHERE team_id = ?1 AND kind = 'worker' AND role_slug = ?2)", params![team, ROLE], |r| r.get(0))
        .map_err(e)
}

/// Eligible reviewer for authorization/gate detection, never `author`. `open` makes the
/// recipient provisional: busy reviewers leave the job in the durable role pool.
pub fn reviewer(conn: &Connection, team: &str, author: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT a.id FROM agents a WHERE a.team_id = ?1 AND a.kind = 'worker' AND a.role_slug = ?2 AND a.id <> ?3
         ORDER BY (SELECT COUNT(*) FROM delegations d WHERE d.to_agent = a.id AND d.status IN ('queued','running')), a.sort, a.id
         LIMIT 1",
        params![team, ROLE, author],
        |r| r.get(0),
    )
    .optional()
    .map_err(e)
}

/// What the merge gate says about `target`'s tip `sha`.
#[derive(Debug, PartialEq)]
pub enum Gate {
    Pass,
    Running(i64),
    Fix { review: i64, findings: String },
    /// No verdict for this commit and no review running; `reviewed`: the commit last reviewed.
    Missing { reviewed: Option<String> },
}

pub fn gate(conn: &Connection, target: i64, sha: &str) -> Result<Gate, String> {
    if let Some(r) = of_sha(conn, target, sha)? {
        match r.verdict.as_deref() {
            Some("onay") => return Ok(Gate::Pass),
            Some(_) => return Ok(Gate::Fix { review: r.review_id, findings: r.findings.unwrap_or_default() }),
            None => {}
        }
    }
    if let Some(id) = running(conn, target)? {
        return Ok(Gate::Running(id));
    }
    Ok(Gate::Missing { reviewed: latest(conn, target)?.map(|r| r.sha).filter(|s| s != sha) })
}

/// #1980: every commit some review approved (`onay`), whichever job it belonged to.
pub fn approved_shas(conn: &Connection) -> Result<Vec<String>, String> {
    let mut st = conn.prepare("SELECT DISTINCT sha FROM delegation_reviews WHERE verdict = 'onay'").map_err(e)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(e)?;
    let mut out = Vec::new();
    for s in rows {
        out.push(s.map_err(e)?);
    }
    Ok(out)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// `None`: the gate lets the merge through. Else the `merged: false` answer; `review.state` `yok`
/// asks the caller to open a review (it has a reviewer to give it to).
pub fn refusal(gate: Gate, has_reviewer: bool, branch: &str, sha: &str) -> Option<Value> {
    let tip = short(sha);
    let (review, note) = match gate {
        Gate::Pass => return None,
        Gate::Running(id) => (
            json!({ "state": "sürüyor", "id": id, "sha": tip }),
            format!("inceleme #{id} sürüyor (dal ucu {tip}); karar gelmeden birleştirme yok. Karar sonuç olarak sana gelir."),
        ),
        Gate::Fix { review, findings } => (
            json!({ "state": "düzelt", "id": review, "sha": tip, "important": findings }),
            format!("inceleme #{review} kararı: düzelt (dal ucu {tip}). Önemli bulguları düzeltme işi olarak aynı worker'a `delegate` ile `fixOf` vererek ver."),
        ),
        Gate::Missing { reviewed } if has_reviewer => {
            let was = reviewed.map(|s| format!(" (son incelenen {})", short(&s))).unwrap_or_default();
            (json!({ "state": "yok", "sha": tip }), format!("dal ucu {tip} incelenmedi{was}; inceleme açılıyor, karar gelmeden birleştirme yok."))
        }
        Gate::Missing { .. } => (
            json!({ "state": "inceleyici yok", "sha": tip }),
            "takımın tek code-review üyesi işin yazarı; inceleme açılamadı. Diff'i patrona göster; patron onaylarsa forceReview: true ile tekrar çağır.".into(),
        ),
    };
    Some(json!({ "merged": false, "branch": branch, "review": review, "note": note }))
}

/// `karar: …` on the first non-blank line, then the `önemli` section. `None`: no valid verdict.
pub fn parse(text: &str) -> Option<(String, String)> {
    let norm = |l: &str| l.trim().trim_matches(|c: char| matches!(c, '#' | '*' | '-' | '_' | '`' | ' ')).to_lowercase();
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    let first = norm(lines.next()?);
    let value = first.strip_prefix("karar")?.trim_start().strip_prefix(':')?;
    let verdict = match value.trim().trim_matches(|c: char| matches!(c, '*' | '`' | '.' | ' ')) {
        "onay" => "onay",
        "düzelt" | "duzelt" => "düzelt",
        _ => return None,
    };
    let mut important = Vec::new();
    let mut inside = false;
    for l in lines {
        let n = norm(l);
        let n = n.trim_end_matches(':');
        if n.starts_with("önemli") || n.starts_with("onemli") {
            inside = true;
            continue;
        }
        if n.starts_with("küçük") || n.starts_with("kucuk") {
            inside = false;
            continue;
        }
        if inside {
            important.push(l.trim_end());
        }
    }
    let mut important = important.join("\n").trim().to_string();
    if important.chars().count() > 2_000 {
        important = important.chars().take(2_000).collect::<String>() + " …";
    }
    Some((verdict.to_string(), important))
}

/// Called from `report`: a review's report must carry a verdict; it is stored with the review.
/// Any other job: nothing.
pub fn record(conn: &Connection, d: &Delegation, summary: &str, result_path: Option<&str>) -> Result<(), String> {
    record_with_content(conn, d, summary, result_path, None)
}

/// Content delivery records the submitted verdict without reopening the mutable archive.
pub(crate) fn record_with_content(conn: &Connection, d: &Delegation, summary: &str, result_path: Option<&str>, content: Option<&str>) -> Result<(), String> {
    if !is_review(conn, d.id)? {
        return Ok(());
    }
    let parsed = if let Some(body) = content {
        parse(body)
    } else {
        let file = result_path.and_then(|p| std::fs::read_to_string(p).ok());
        file.as_deref().and_then(parse).or_else(|| parse(summary))
    };
    let Some((verdict, findings)) = parsed else {
        return Err(format!(
            "İnceleme raporunun ilk satırı `karar: onay` ya da `karar: düzelt` olmalı (sonra `## önemli` / `## küçük`). Raporu düzelt ve report'u tekrar çağır. Devretme #{} açık kaldı.",
            d.id
        ));
    };
    conn.execute(
        "UPDATE delegation_reviews SET verdict = ?2, findings = ?3 WHERE review_id = ?1",
        params![d.id, verdict, (!findings.is_empty()).then_some(findings)],
    )
    .map_err(e)?;
    // Biten inceleme panoda "Sürüyor"da kalmaz.
    finish_task(conn, d.id)?;
    Ok(())
}

/// İnceleme devretmesi kapandı (raporlu, raporsuz ya da başarısız): bağlı pano görevi
/// hâlâ `doing` ise `done` olur. İnceleme değilse ya da görev yoksa/silinmişse no-op.
pub(crate) fn finish_task(conn: &Connection, review_id: i64) -> Result<(), String> {
    if !is_review(conn, review_id)? {
        return Ok(());
    }
    let task = delegate::get(conn, review_id)?.task_id;
    let Some(task) = task else { return Ok(()) };
    let Ok(t) = crate::db::tasks::get(conn, task) else { return Ok(()) };
    if t.status == "doing" {
        crate::db::tasks::update(conn, task, crate::db::tasks::TaskPatch { status: Some("done".into()), ..Default::default() })?;
    }
    Ok(())
}

/// What opening a review for a job's tip did.
#[derive(Debug)]
pub enum Opened {
    /// The review job (queued; the caller dispatches it).
    Started(Delegation),
    NoReviewer,
    Running(i64),
    /// This commit already has a verdict.
    Reviewed,
}

/// Opens the review of `target`'s branch at `sha` (`team`: the team folder's branch): a board task
/// and a queued job for the reviewer. Git ran before (the db lock is held here).
pub fn open(conn: &Connection, target: &Delegation, w: &Wt, sha: &str, team: &str) -> Result<Opened, String> {
    if of_sha(conn, target.id, sha)?.is_some_and(|r| r.verdict.is_some()) {
        return Ok(Opened::Reviewed);
    }
    if let Some(id) = running(conn, target.id)? {
        return Ok(Opened::Running(id));
    }
    let Some(to) = reviewer(conn, &target.team_id, &target.to_agent)? else { return Ok(Opened::NoReviewer) };
    let leader = delegate::caller(conn, &target.from_agent)?;
    // #888: the reviewed job's own binding, as `fixOf` inherits it — not a selection (several
    // projects would need one) nor the team's project of today. Resolved before any write.
    let context = delegate::resolve_context(conn, std::path::Path::new(""), &leader, None, Some(target.id))?;
    let prompt = prompt(conn, target, w, sha, team)?;
    let title = review_title(conn, target);
    // Board task, review job and its record land together or not at all.
    let tx = conn.unchecked_transaction().map_err(e)?;
    let conn = &tx;
    let task = crate::db::tasks::create(
        conn,
        crate::db::tasks::NewTask {
            title,
            description: format!("Dex'in otomatik kod incelemesi: #{} dalı {} @ {}", target.id, w.branch, short(sha)),
            status: Some("doing".into()),
            agent_id: Some(to.clone()),
            team_id: Some(target.team_id.clone()),
        },
    )?;
    conn.execute("UPDATE tasks SET project_id = ?2 WHERE id = ?1", params![task.id, context.project_id]).map_err(e)?;
    let d = delegate::insert_bound(
        conn,
        delegate::NewDelegation {
            from: &leader,
            to_agent: to,
            team_id: target.team_id.clone(),
            service_team_id: None,
            prompt,
            task_id: Some(task.id),
            patron_words: origin_words(conn, target)?,
            effort: None,
        },
        &context,
    )?;
    conn.execute(
        "INSERT INTO delegation_reviews (review_id, target_id, sha, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![d.id, target.id, sha, now()],
    )
    .map_err(e)?;
    delegate::queue_role(conn, d.id, ROLE, Some(&target.to_agent))?;
    let d = delegate::assign_role_job(conn, d.id)?;
    tx.commit().map_err(e)?;
    Ok(Opened::Started(d))
}

/// İnceleme kartının başlığı: incelenen işin pano görev başlığı, yoksa plan/delegate
/// `title`'ı, o da yoksa istemin ilk ANLAMLI satırı. Numara her durumda baştadır.
pub(crate) fn review_title(conn: &Connection, target: &Delegation) -> String {
    let name = target
        .task_id
        .and_then(|t| crate::db::tasks::get(conn, t).ok())
        .map(|t| t.title)
        .or_else(|| plan_item_title(conn, target.id))
        .unwrap_or_else(|| first_meaningful_line(&target.prompt, 80));
    format!("İnceleme #{}: {}", target.id, name)
}

/// İncelenen devretmeyi başlatan plan öğesinin `title`'ı (planlı işler).
fn plan_item_title(conn: &Connection, delegation: i64) -> Option<String> {
    conn.query_row("SELECT title FROM plan_items WHERE delegation_id = ?1 ORDER BY id DESC LIMIT 1", [delegation], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

/// İstemden ilk anlamlı satır: boş satırlar, markdown başlıkları (`#`, `## Amaç` gibi
/// biçim başlıkları) ve "Patronun sözü" satırı atlanır. Bulunamazsa boş döner.
/// Pano kartı başlığı için de kullanılır (#1768: `delegate` otomatik kartı).
pub(crate) fn first_meaningful_line(s: &str, max: usize) -> String {
    let l = s
        .lines()
        .map(str::trim)
        .filter(|l| {
            if l.is_empty() {
                return false;
            }
            let low = l.to_lowercase();
            if low.contains("patronun sözü") {
                return false;
            }
            let bare = l.trim_start_matches('#').trim();
            // `#…` başlığı ya da `#` kırpılınca boşalan satır (örn. `##`).
            if bare.len() != l.len() || bare.is_empty() {
                return false;
            }
            true
        })
        .next()
        .unwrap_or("");
    if l.chars().count() > max { l.chars().take(max).collect::<String>() + "…" } else { l.to_string() }
}

/// `target` and the jobs it fixes, newest first.
fn chain(conn: &Connection, target: &Delegation) -> Result<Vec<Delegation>, String> {
    let mut out = vec![target.clone()];
    while let Some(prev) = fix_of(conn, out.last().unwrap().id)? {
        if out.iter().any(|d| d.id == prev) || out.len() > 20 {
            break;
        }
        out.push(delegate::get(conn, prev)?);
    }
    Ok(out)
}

/// The patron's words behind the work: the fix job's own, else the job it fixes.
fn origin_words(conn: &Connection, target: &Delegation) -> Result<Option<String>, String> {
    Ok(chain(conn, target)?.into_iter().find_map(|d| delegate::patron_words(d.patron_words.as_deref())))
}

fn prompt(conn: &Connection, target: &Delegation, w: &Wt, sha: &str, team: &str) -> Result<String, String> {
    // #1756: Bash drops unquoted backslashes (`C:\w` → `C:w`), so a Windows `display()` path in
    // `-C` is read — by the shell and the read-only parser alike — as a relative path outside the
    // reviewer's roots and the command is denied. Forward slashes, quoted for spaces.
    let root = format!("\"{}\"", w.root.to_string_lossy().replace('\\', "/"));
    let author = delegate::caller(conn, &target.to_agent).map(|c| c.name).unwrap_or_else(|_| target.to_agent.clone());
    let report = match &target.result_path {
        Some(p) => p.clone(),
        None => format!("dosya yok; worker'ın özeti: {}", target.summary.as_deref().unwrap_or("-")),
    };
    let file = w.root.join("REVIEW.md");
    let criteria = if file.is_file() {
        format!("Ölçütler: {} — oku ve ona göre incele.", file.display())
    } else {
        format!("Ölçütler (takım klasöründe REVIEW.md yok, Dex'inki):\n{}", CRITERIA.trim())
    };
    let mut s = format!(
        "Dex otomatik kod incelemesi: #{id} işinin dalı takım dalına birleşmeden önce.\n\
         - Dal: {branch} @ {sha} (yazan: {author})\n\
         - Diff: git -C {root} diff {team}...{sha}\n\
         - Commit'ler: git -C {root} log --oneline {team}..{sha}\n\
         - Worker raporu: {report}\n\
         - {criteria}\n\n\
         Kod yazma, commit atma, dal değiştirme; yalnız oku. Karar yalnız {short} commit'i için geçerli.\n\
         Rapor (resultPath dosyası) biçimi: ilk satır `karar: onay` ya da `karar: düzelt`; sonra `## önemli` ve `## küçük` \
         başlıkları altında her bulgu `- dosya:satır — kısa gerekçe`. Önemli bulgu varsa karar düzelt; bulgu yoksa başlığa \"yok\" yaz.\n",
        id = target.id,
        branch = w.branch,
        short = short(sha),
    );
    let chain = chain(conn, target)?;
    for (i, d) in chain.iter().enumerate() {
        let head = match (i, chain.len()) {
            (0, 1) => format!("Asıl görev (#{})", d.id),
            (0, _) => format!("Düzeltme işi (#{})", d.id),
            (_, n) if i == n - 1 => format!("Asıl görev (#{})", d.id),
            _ => format!("Önceki düzeltme (#{})", d.id),
        };
        s.push_str(&format!("\n## {head}\n{}\n", d.prompt.trim_end()));
    }
    Ok(s)
}

/// For the leader's result lines: a review's verdict and important findings, or a job's review.
pub fn result_line(conn: &Connection, d: &Delegation) -> Option<String> {
    if let Ok(Some(r)) = get(conn, d.id) {
        let verdict = r.verdict.as_deref().unwrap_or("yok (rapor verilmedi)");
        let mut s = format!("[{}] #{} @ {} · {}", crate::dexmsg::INCELEME, r.target_id, short(&r.sha), verdict);
        if let Some(f) = r.findings.as_deref().filter(|f| !f.is_empty()) {
            s.push_str(&format!("\n  önemli:\n    {}", f.replace('\n', "\n    ")));
        }
        match r.verdict.as_deref() {
            Some("onay") => s.push_str(&format!("\n  → delegation_merge #{} ile birleştir", r.target_id)),
            Some(_) => s.push_str(&format!("\n  → düzeltme işini aynı worker'a `delegate` ile `fixOf: {}` vererek ver", r.target_id)),
            None => {}
        }
        return Some(s);
    }
    let r = latest(conn, d.id).ok().flatten()?;
    let who = delegate::get(conn, r.review_id).ok().and_then(|rd| delegate::caller(conn, &rd.to_agent).ok()).map(|c| c.name).unwrap_or_default();
    Some(format!("[{}] #{} · karar bekleniyor ({who}) — karar `onay` gelmeden birleştirme yok", crate::dexmsg::INCELEME, r.review_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_role_pool_waits_and_never_assigns_the_author() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        c.execute(
            "UPDATE agents SET role_slug=?1 WHERE id IN ('kaan','ayla')",
            [ROLE],
        )
        .unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let job = |to: &str| {
            delegate::insert(
                &c,
                delegate::NewDelegation {
                    from: &lead,
                    to_agent: to.into(),
                    team_id: lead.team_id.clone(),
                    service_team_id: None,
                    prompt: "work".into(),
                    task_id: None,
                    patron_words: None,
                    effort: None,
                },
            )
            .unwrap()
        };
        let author = job("kaan");
        delegate::set_status(&c, author.id, "done").unwrap();
        let busy = job("ayla");
        delegate::set_status(&c, busy.id, "running").unwrap();
        let w = Wt {
            delegation_id: author.id,
            agent_id: "kaan".into(),
            root: std::path::PathBuf::new(),
            path: std::path::PathBuf::new(),
            branch: "dex/kaan/1".into(),
            base: "base".into(),
            junction: false,
            merged_at: None,
            removed_at: None,
            merge_branch: None,
        };
        let Opened::Started(review) = open(&c, &author, &w, "abc12345", "team").unwrap() else {
            panic!("review not opened")
        };
        assert!(
            review.to_agent.is_empty(),
            "a busy reviewer does not own the review yet"
        );
        assert!(
            delegate::next_for_worker(&c, "kaan").unwrap().is_none(),
            "author is free but excluded"
        );
        delegate::set_status(&c, busy.id, "done").unwrap();
        let assigned = delegate::next_for_worker(&c, "ayla").unwrap().unwrap();
        assert_eq!(
            (assigned.id, assigned.to_agent.as_str()),
            (review.id, "ayla")
        );
    }

    #[test]
    fn finished_review_closes_its_board_task_and_titles_from_the_job() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        c.execute("UPDATE agents SET role_slug=?1 WHERE id='ayla'", [ROLE]).unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let task = crate::db::tasks::create(
            &c,
            crate::db::tasks::NewTask {
                title: "Gerçek iş adı".into(),
                description: String::new(),
                status: None,
                agent_id: None,
                team_id: Some("default".into()),
            },
        )
        .unwrap();
        let target = delegate::insert(
            &c,
            delegate::NewDelegation {
                from: &lead,
                to_agent: "kaan".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "## Amaç\nGerçek iş adı\nayrıntı".into(),
                task_id: Some(task.id),
                patron_words: None,
                effort: None,
            },
        )
        .unwrap();
        delegate::set_status(&c, target.id, "done").unwrap();
        let w = Wt {
            delegation_id: target.id,
            agent_id: "kaan".into(),
            root: std::path::PathBuf::new(),
            path: std::path::PathBuf::new(),
            branch: "dex/kaan/1".into(),
            base: "base".into(),
            junction: false,
            merged_at: None,
            removed_at: None,
            merge_branch: None,
        };
        let Opened::Started(review) = open(&c, &target, &w, "abc12345", "team").unwrap() else {
            panic!("review not opened")
        };
        let rt = crate::db::tasks::get(&c, review.task_id.unwrap()).unwrap();
        assert_eq!(rt.title, format!("İnceleme #{}: Gerçek iş adı", target.id));
        assert_eq!(rt.status, "doing");
        let lens = delegate::caller(&c, "ayla").unwrap();
        delegate::report(&c, &lens, review.id, "karar: onay\n## önemli\nyok\n", None).unwrap();
        assert_eq!(crate::db::tasks::get(&c, rt.id).unwrap().status, "done");
        assert_eq!(get(&c, review.id).unwrap().unwrap().verdict.as_deref(), Some("onay"));
    }

    #[test]
    fn review_title_skips_format_headings_and_falls_back_to_plan_title() {
        assert_eq!(first_meaningful_line("## Amaç\n\n# Başlık\n  \nson satır", 80), "son satır");
        assert_eq!(first_meaningful_line("## Patronun sözü\nbi şey yap\n\n## Liderin açıklaması\nasıl iş", 80), "bi şey yap");
        assert_eq!(first_meaningful_line("## Amaç", 80), "");
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        c.execute("UPDATE agents SET role_slug=?1 WHERE id='ayla'", [ROLE]).unwrap();
        let lead = delegate::caller(&c, "timur").unwrap();
        let target = delegate::insert(
            &c,
            delegate::NewDelegation {
                from: &lead,
                to_agent: "kaan".into(),
                team_id: "default".into(),
                service_team_id: None,
                prompt: "## Amaç\nPlanlı işin satırı".into(),
                task_id: None,
                patron_words: None,
                effort: None,
            },
        )
        .unwrap();
        c.execute("INSERT INTO plans(team_id,leader,title,status,created_at,updated_at) VALUES('default','timur','p','active',1,1)", [])
            .unwrap();
        c.execute(
            "INSERT INTO plan_items(plan_id,key,title,prompt,target,depends_on,status,delegation_id) VALUES(1,'A','Plan iş başlığı','p','kaan','[]','running',?1)",
            [target.id],
        )
        .unwrap();
        // Plan başlığı, istemin ilk anlamlı satırından önce gelir.
        assert_eq!(review_title(&c, &target), format!("İnceleme #{}: Plan iş başlığı", target.id));
        c.execute("DELETE FROM plan_items", []).unwrap();
        // Biçim başlıkları atlanır: "## Amaç" değil, altındaki satır.
        assert_eq!(review_title(&c, &target), format!("İnceleme #{}: Planlı işin satırı", target.id));
    }

    #[test]
    fn verdict_is_the_first_line_and_important_findings_are_collected() {
        let (v, f) = parse("\n**karar: düzelt**\n\n## önemli\n- a.rs:3 — taşma\n- b.rs:9 — sır log'da\n\n## küçük\n- c.rs:1 — ad\n").unwrap();
        assert_eq!(v, "düzelt");
        assert_eq!(f, "- a.rs:3 — taşma\n- b.rs:9 — sır log'da");
        let (v, f) = parse("Karar: Onay\nÖnemli:\nyok\nKüçük:\n- x.rs:1 — ad").unwrap();
        assert_eq!((v.as_str(), f.as_str()), ("onay", "yok"));
        assert!(parse("bitti: inceledim\nkarar: onay").is_none(), "the verdict leads");
        assert!(parse("karar: belki").is_none());
        assert!(parse("").is_none());
    }
}
