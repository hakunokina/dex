//! The patron's notes (DEX-41): one Markdown body per scope — `shared` (every team sees it) or a
//! team id. Personal to the patron: never part of an agent's prompt or memory.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub const SHARED: &str = "shared";

/// Plenty for hand-written notes; stops a runaway paste from bloating every `notes-changed`.
const MAX_BYTES: usize = 1 << 20;

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub scope: String,
    pub body: String,
    /// 0 = never written.
    pub updated_at: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `shared` or an existing team's id (team ids always carry a `-xxxx` suffix or are `default`).
fn check_scope(conn: &Connection, scope: &str) -> Result<(), String> {
    if scope == SHARED {
        return Ok(());
    }
    conn.query_row("SELECT 1 FROM teams WHERE id = ?1", [scope], |_| Ok(()))
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("not kapsamı yok: {scope}"))
}

pub fn get(conn: &Connection, scope: &str) -> Result<Note, String> {
    check_scope(conn, scope)?;
    let row = conn
        .query_row("SELECT body, updated_at FROM notes WHERE scope = ?1", [scope], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(|e| e.to_string())?;
    let (body, updated_at) = row.unwrap_or_default();
    Ok(Note { scope: scope.into(), body, updated_at })
}

pub fn set(conn: &Connection, scope: &str, body: &str) -> Result<Note, String> {
    check_scope(conn, scope)?;
    if body.len() > MAX_BYTES {
        return Err("not çok uzun (en çok 1 MB)".into());
    }
    let at = now();
    conn.execute(
        "INSERT INTO notes (scope, body, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(scope) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
        params![scope, body, at],
    )
    .map_err(|e| e.to_string())?;
    Ok(Note { scope: scope.into(), body: body.into(), updated_at: at })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        crate::db::open(std::path::Path::new(":memory:")).unwrap()
    }

    #[test]
    fn empty_then_roundtrip_per_scope() {
        let c = db();
        assert_eq!(get(&c, SHARED).unwrap(), Note { scope: SHARED.into(), body: String::new(), updated_at: 0 });
        let s = set(&c, SHARED, "# Ortak\n- a").unwrap();
        assert!(s.updated_at > 0);
        set(&c, "default", "takım notu").unwrap();
        assert_eq!(get(&c, SHARED).unwrap().body, "# Ortak\n- a");
        assert_eq!(get(&c, "default").unwrap().body, "takım notu");
        set(&c, SHARED, "").unwrap();
        assert_eq!(get(&c, SHARED).unwrap().body, "", "boşaltmak da yazımdır");
        assert_eq!(get(&c, "default").unwrap().body, "takım notu", "kapsamlar ayrı");
    }

    #[test]
    fn unknown_scope_and_oversize_are_refused() {
        let c = db();
        assert!(get(&c, "yok-0000").unwrap_err().contains("kapsamı yok"));
        assert!(set(&c, "yok-0000", "x").is_err());
        assert!(set(&c, SHARED, &"x".repeat(MAX_BYTES + 1)).unwrap_err().contains("uzun"));
        assert_eq!(get(&c, SHARED).unwrap().updated_at, 0, "reddedilen yazım kayıt açmaz");
    }

    #[test]
    fn team_notes_go_with_the_team() {
        let c = db();
        c.execute("INSERT INTO teams (id, name, kind, created_at) VALUES ('t-0001', 'T', 'service', 0)", []).unwrap();
        set(&c, "t-0001", "gider").unwrap();
        set(&c, SHARED, "kalır").unwrap();
        c.execute("DELETE FROM teams WHERE id = 't-0001'", []).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(get(&c, SHARED).unwrap().body, "kalır");
    }
}
