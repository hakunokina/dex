//! #1858: the patron's daily three lines. Once a day (04:30 local, or the first tick after it)
//! the leader gets a `[Dex-Motivasyon]` turn and saves them with `motivation_set`; the phone
//! pulls them over the host RPC (`motivation_today`) and reports delivery (`motivation_result`).
//! No push: no PC, no text. The lines are personal data: never logged.
use crate::db::with;
use crate::inbox::Error;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};

// 41: additive, idempotent (IF NOT EXISTS in a versioned migration). Before commit: ROLLBACK.
// After commit: keep the table; revert only the application. Full undo (drops the lines):
// DROP TABLE motivations; DELETE FROM settings WHERE key='motivation_day';
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS motivations (
    date TEXT PRIMARY KEY,
    morning TEXT NOT NULL,
    noon TEXT NOT NULL,
    evening TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    delivered_at INTEGER,
    ok INTEGER,
    error TEXT
);";

pub const LEADER: &str = "timur";
pub const MAX_CHARS: usize = 200;
pub const MAX_ERROR_CHARS: usize = 500;
/// Local time from which the day's turn is due.
pub fn due_at() -> NaiveTime {
    NaiveTime::from_hms_opt(4, 30, 0).unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Motivation {
    pub date: String,
    pub morning: String,
    pub noon: String,
    pub evening: String,
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

pub fn prompt(date: &str) -> String {
    crate::dexmsg::line(crate::dexmsg::MOTIVASYON, format!(
        "Patron için bugünün ({date}) 3 motivasyon cümlesini yaz ve motivation_set ile kaydet: sabah (05–12), öğle (12–19), akşam (19–05). Her biri tek satır, en çok {MAX_CHARS} karakter."
    ))
}

/// All three or none: every violation is listed, nothing is written.
pub fn validate(lines: [(&str, &str); 3]) -> Result<(), String> {
    let mut bad = Vec::new();
    for (name, text) in lines {
        let n = text.trim().chars().count();
        if n == 0 {
            bad.push(format!("{name} boş"));
        } else if text.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
            bad.push(format!("{name} tek satır olmalı (satır sonu/kontrol karakteri var)"));
        } else if n > MAX_CHARS {
            bad.push(format!("{name} en çok {MAX_CHARS} karakter ({n})"));
        }
    }
    if bad.is_empty() { Ok(()) } else { Err(format!("motivation_set reddedildi, hiçbiri kaydedilmedi: {}", bad.join("; "))) }
}

/// Upsert for `date`; a rewrite clears the delivery state (the phone delivers it again).
pub fn set(c: &Connection, date: &str, morning: &str, noon: &str, evening: &str, now: i64) -> Result<(), String> {
    validate([("morning", morning), ("noon", noon), ("evening", evening)])?;
    c.execute(
        "INSERT INTO motivations(date,morning,noon,evening,created_at) VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(date) DO UPDATE SET morning=excluded.morning,noon=excluded.noon,evening=excluded.evening,
            created_at=excluded.created_at,delivered_at=NULL,ok=NULL,error=NULL",
        params![date, morning.trim(), noon.trim(), evening.trim(), now],
    )
    .map(|_| ())
    .map_err(e)
}

pub fn get(c: &Connection, date: &str) -> Result<Option<Motivation>, String> {
    c.query_row("SELECT date,morning,noon,evening FROM motivations WHERE date=?1", [date], |r| {
        Ok(Motivation { date: r.get(0)?, morning: r.get(1)?, noon: r.get(2)?, evening: r.get(3)? })
    })
    .optional()
    .map_err(e)
}

/// `false` when no row exists for `date`.
pub fn record_result(c: &Connection, date: &str, ok: bool, error: Option<&str>, now: i64) -> Result<bool, String> {
    c.execute("UPDATE motivations SET delivered_at=?2,ok=?3,error=?4 WHERE date=?1", params![date, now, ok, error])
        .map(|n| n == 1)
        .map_err(e)
}

/// Due at/after 04:30 of `now`'s day, at most once per day across restarts (claimed in settings,
/// the maintenance_day pattern). `Some(date)` = this call claimed it.
pub fn claim(c: &Connection, now: NaiveDateTime) -> Result<Option<String>, String> {
    if now.time() < due_at() {
        return Ok(None);
    }
    let day = now.date().format("%Y-%m-%d").to_string();
    let n = c
        .execute(
            "INSERT INTO settings(key,value) VALUES ('motivation_day',?1)
            ON CONFLICT(key) DO UPDATE SET value=excluded.value WHERE settings.value < excluded.value",
            [serde_json::to_string(&day).map_err(|e| e.to_string())?],
        )
        .map_err(e)?;
    Ok((n == 1).then_some(day))
}

fn today() -> String {
    chrono::Local::now().date_naive().format("%Y-%m-%d").to_string()
}

fn now_ms() -> i64 {
    crate::diagnostics::now_ms() as i64
}

impl crate::Core {
    /// Maintenance-loop tick: claims the day and queues the leader turn (a leader note woken by
    /// the usual paths: idle → paste, busy → its Stop, sleeping → resume, closed → next start).
    pub(crate) fn motivation_tick(&self, now: NaiveDateTime) -> Result<bool, String> {
        let claimed = with(&self.db, |c| {
            let tx = c.unchecked_transaction().map_err(e)?;
            let Some(day) = claim(&tx, now)? else { return Ok(false) };
            crate::plan::note(&tx, LEADER, &prompt(&day))?;
            tx.commit().map_err(e)?;
            Ok(true)
        })?;
        if claimed {
            crate::diagnostics::event("motivation", json!({"reason":"asked"}));
            self.leader_notes_wake(LEADER);
        }
        Ok(claimed)
    }

    /// The leader's `motivation_set` (MCP only; the leader check is the MCP's): today's date in
    /// dexd local time.
    pub fn motivation_set(&self, morning: &str, noon: &str, evening: &str) -> Result<Value, String> {
        let date = today();
        with(&self.db, |c| set(c, &date, morning, noon, evening, now_ms()))?;
        Ok(json!({ "ok": true, "date": date }))
    }

    /// Host RPC: today's lines or `null`.
    pub fn motivation_today(&self) -> Result<Option<Motivation>, Error> {
        with(&self.db, |c| get(c, &today()))
            .map_err(|_| Error { code: "motivation_unavailable", message: "motivasyon okunamadı".into() })
    }

    /// Host RPC: the phone's delivery result for `date`.
    pub fn motivation_result(&self, date: &str, ok: bool, error: Option<&str>) -> Result<Value, Error> {
        if NaiveDate::parse_from_str(date, "%Y-%m-%d").map(|d| d.format("%Y-%m-%d").to_string() != date).unwrap_or(true) {
            return Err(Error { code: "motivation_date", message: "tarih yyyy-MM-dd olmalı".into() });
        }
        if error.is_some_and(|s| s.chars().count() > MAX_ERROR_CHARS) {
            return Err(Error { code: "motivation_error_long", message: format!("error en çok {MAX_ERROR_CHARS} karakter") });
        }
        let found = with(&self.db, |c| record_result(c, date, ok, error, now_ms()))
            .map_err(|_| Error { code: "motivation_unavailable", message: "sonuç kaydedilemedi".into() })?;
        if !found {
            return Err(Error { code: "motivation_not_found", message: format!("{date} için motivasyon yok") });
        }
        Ok(json!({ "ok": true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn db_path(tag: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("motivation-{tag}-{}-{}", std::process::id(), crate::agents::next_seq()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("dex.db")
    }

    fn core(tag: &str) -> std::sync::Arc<crate::Core> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("motivation-core-{tag}-{}-{}", std::process::id(), crate::agents::next_seq()));
        crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap()
    }

    fn row(c: &Connection, date: &str) -> (String, Option<i64>, Option<bool>, Option<String>) {
        c.query_row("SELECT morning,delivered_at,ok,error FROM motivations WHERE date=?1", [date], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap()
    }

    #[test]
    fn any_violation_rejects_the_whole_set() {
        let path = db_path("rules");
        let c = crate::db::open(&path).unwrap();
        let long = "ç".repeat(MAX_CHARS + 1);
        for (m, n, ev) in [("", "öğle", "akşam"), ("  ", "öğle", "akşam"), ("sabah", "iki\nsatır", "akşam"), ("sabah", "öğle", "a\rb"), ("sabah", "a\u{2028}b", "akşam"), ("a\u{2029}b", "öğle", "akşam"),("sabah", "öğle", long.as_str())] {
            let err = set(&c, "2026-10-15", m, n, ev, 1).unwrap_err();
            assert!(err.contains("hiçbiri kaydedilmedi"), "{err}");
        }
        let err = validate([("morning", ""), ("noon", "a\nb"), ("evening", &long)]).unwrap_err();
        assert!(err.contains("morning boş") && err.contains("noon tek satır") && err.contains("evening en çok 200"), "{err}");
        assert_eq!(get(&c, "2026-10-15").unwrap(), None);
        // Exactly 200 (multi-byte) is fine.
        set(&c, "2026-10-15", &"ç".repeat(MAX_CHARS), "öğle", "akşam", 1).unwrap();
        assert!(get(&c, "2026-10-15").unwrap().is_some());
    }

    #[test]
    fn same_day_overwrites_and_clears_delivery() {
        let path = db_path("overwrite");
        let c = crate::db::open(&path).unwrap();
        set(&c, "2026-10-15", " ilk ", "öğle", "akşam", 1).unwrap();
        assert_eq!(get(&c, "2026-10-15").unwrap().unwrap().morning, "ilk");
        assert!(record_result(&c, "2026-10-15", false, Some("launcher yok"), 2).unwrap());
        assert_eq!(row(&c, "2026-10-15"), ("ilk".into(), Some(2), Some(false), Some("launcher yok".into())));
        set(&c, "2026-10-15", "ikinci", "öğle2", "akşam2", 3).unwrap();
        assert_eq!(row(&c, "2026-10-15"), ("ikinci".into(), None, None, None));
        let n: i64 = c.query_row("SELECT count(*) FROM motivations", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert!(!record_result(&c, "2026-10-14", true, None, 4).unwrap());
    }

    #[test]
    fn one_trigger_per_day_across_reopens() {
        let path = db_path("claim");
        {
            let c = crate::db::open(&path).unwrap();
            // Before 04:30: not due.
            assert_eq!(claim(&c, at("2026-10-15 02:00")).unwrap(), None);
            assert_eq!(claim(&c, at("2026-10-15 04:29")).unwrap(), None);
            // Opened late (PC was off at 04:30): due once.
            assert_eq!(claim(&c, at("2026-10-15 10:00")).unwrap(), Some("2026-10-15".into()));
            assert_eq!(claim(&c, at("2026-10-15 10:01")).unwrap(), None);
        }
        // Second opening the same day (migrations run again — idempotent): no repeat.
        let c = crate::db::open(&path).unwrap();
        assert_eq!(claim(&c, at("2026-10-15 23:59")).unwrap(), None);
        // Next day after midnight but before 04:30: still nothing; at 04:30 once.
        assert_eq!(claim(&c, at("2026-10-16 00:10")).unwrap(), None);
        assert_eq!(claim(&c, at("2026-10-16 04:30")).unwrap(), Some("2026-10-16".into()));
        // A clock moved back never re-asks an earlier day.
        assert_eq!(claim(&c, at("2026-10-15 12:00")).unwrap(), None);
    }

    #[test]
    fn tick_queues_one_tagged_leader_note() {
        let core = core("tick");
        assert!(!core.motivation_tick(at("2026-10-15 03:00")).unwrap());
        assert!(core.motivation_tick(at("2026-10-15 09:00")).unwrap());
        assert!(!core.motivation_tick(at("2026-10-15 09:01")).unwrap());
        let notes: Vec<String> = with(&core.db, |c| {
            let mut st = c.prepare("SELECT text FROM leader_notes WHERE agent_id='timur'").map_err(e)?;
            let rows = st.query_map([], |r| r.get(0)).map_err(e)?.collect::<Result<Vec<String>, _>>().map_err(e)?;
            Ok(rows)
        }).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("[Dex-Motivasyon] Patron için bugünün (2026-10-15) 3 motivasyon"), "{}", notes[0]);
        assert!(crate::dexmsg::is_dex_turn(&notes[0]));
        assert_eq!(crate::plan::tag_note_text(&notes[0]), notes[0], "tagged note is not re-tagged as Dex-Not");
        core.shutdown();
    }

    #[test]
    fn today_and_result_flow() {
        let core = core("flow");
        assert_eq!(core.motivation_today().unwrap(), None);
        let set = core.motivation_set("sabah", "öğle", "akşam").unwrap();
        let date = set["date"].as_str().unwrap().to_string();
        assert_eq!(date, today());
        let got = core.motivation_today().unwrap().unwrap();
        assert_eq!(serde_json::to_value(&got).unwrap(), json!({"date": date, "morning": "sabah", "noon": "öğle", "evening": "akşam"}));
        assert_eq!(core.motivation_result(&date, true, None).unwrap(), json!({"ok": true}));
        assert_eq!(core.motivation_result("2026-1-5", true, None).unwrap_err().code, "motivation_date");
        assert_eq!(core.motivation_result("2000-01-01", true, None).unwrap_err().code, "motivation_not_found");
        assert_eq!(core.motivation_result(&date, false, Some(&"x".repeat(MAX_ERROR_CHARS + 1))).unwrap_err().code, "motivation_error_long");
        assert!(core.motivation_result(&date, false, Some(&"x".repeat(MAX_ERROR_CHARS))).is_ok());
        let r = with(&core.db, |c| Ok(row(c, &date))).unwrap();
        assert_eq!((r.2, r.3.map(|s| s.len())), (Some(false), Some(MAX_ERROR_CHARS)));
        assert!(r.1.is_some());
        core.shutdown();
    }
}
