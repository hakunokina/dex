//! App-wide settings (DEX-31): `settings` rows, value = JSON. A missing or unreadable row is
//! the built-in default. First (and so far only) key: agent resource limits.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::proc::{default_limits, with_env, Limits};

const LIMITS: &str = "limits";

/// Under this a memory ceiling only crashes agents (claude alone commits several hundred MB).
const MIN_MEM_BYTES: u64 = 1 << 30;

/// Stored limits (what the Settings panel edits) and what a spawn uses after env overrides.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LimitsView {
    pub stored: Limits,
    pub effective: Limits,
    /// Env variables overriding `stored` in this Dex process.
    pub env: Vec<&'static str>,
    /// Only Windows enforces them (Job Objects); elsewhere the values are kept but not applied.
    pub enforced: bool,
}

pub fn limits(conn: &Connection) -> Limits {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [LIMITS], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    raw.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_else(default_limits)
}

pub fn limits_view(conn: &Connection) -> LimitsView {
    let stored = limits(conn);
    let (effective, env) = with_env(stored);
    LimitsView { stored, effective, env, enforced: cfg!(windows) }
}

pub fn set_limits(conn: &Connection, l: Limits) -> Result<LimitsView, String> {
    if l.total_cpu_percent > 100 || l.agent_cpu_percent > 100 {
        return Err("CPU yüzdesi 0–100 olmalı".into());
    }
    if l.mem_bytes != 0 && l.mem_bytes < MIN_MEM_BYTES {
        return Err("bellek tavanı ya kapalı (0) ya da en az 1 GiB olmalı".into());
    }
    let v = serde_json::to_string(&l).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![LIMITS, v],
    )
    .map_err(|e| e.to_string())?;
    Ok(limits_view(conn))
}

// ---- #222: izinli git push hedefleri (literal-URL desenleri, `*` joker) ----

const PUSH_ALLOW: &str = "push_allow";

/// Patronun izin verdiği push hedef desenleri (`permit::allowed_push` eşleştirir). Satır yoksa ya
/// da bozuksa boş liste = eski davranış (tüm push yasak). Yazan yalnız patron (SQL); ajanın
/// dex.db'ye yazı yolu yok (yazı kökleri dışında) ve dexd RPC token'ı secrettir.
pub fn push_allow(conn: &Connection) -> Vec<String> {
    let raw: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [PUSH_ALLOW], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    raw.and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok()).unwrap_or_default()
}

// ---- FIX-3: first-reply thresholds (seconds per provider, 0 = off) ----

const FIRST_REPLY: &str = "first_reply_secs";

/// Built in: only the local LLM (QA-2: a model switch held the first turn 3 min+); hosted
/// providers answer within seconds or fail with StopFailure.
fn default_first_reply(provider: &str) -> u32 {
    if provider == "local" { 90 } else { 0 }
}

fn first_reply_stored(conn: &Connection) -> std::collections::BTreeMap<String, u32> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [FIRST_REPLY], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

pub fn first_reply_secs(conn: &Connection, provider: &str) -> u32 {
    first_reply_stored(conn).get(provider).copied().unwrap_or_else(|| default_first_reply(provider))
}

/// Effective threshold of every known provider.
pub fn first_reply_view(conn: &Connection) -> std::collections::BTreeMap<String, u32> {
    crate::providers::PROVIDERS.iter().map(|p| (p.id.to_string(), first_reply_secs(conn, p.id))).collect()
}

pub fn set_first_reply(conn: &Connection, provider: &str, secs: u32) -> Result<std::collections::BTreeMap<String, u32>, String> {
    crate::providers::get(provider).ok_or_else(|| format!("bilinmeyen sağlayıcı: {provider}"))?;
    if secs != 0 && !(10..=3600).contains(&secs) {
        return Err("ilk yanıt eşiği ya kapalı (0) ya da 10–3600 sn olmalı".into());
    }
    let mut stored = first_reply_stored(conn);
    stored.insert(provider.into(), secs);
    let v = serde_json::to_string(&stored).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![FIRST_REPLY, v],
    )
    .map_err(|e| e.to_string())?;
    Ok(first_reply_view(conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_allow_defaults_empty_and_roundtrips() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert!(push_allow(&c).is_empty(), "satır yok → kapalı");
        c.execute("INSERT INTO settings (key, value) VALUES ('push_allow', '[\"a\",\"b\"]')", []).unwrap();
        assert_eq!(push_allow(&c), vec!["a".to_string(), "b".to_string()]);
        c.execute("UPDATE settings SET value = 'bozuk' WHERE key = 'push_allow'", []).unwrap();
        assert!(push_allow(&c).is_empty(), "bozuk satır → kapalı");
    }

    #[test]
    fn first_reply_defaults_to_local_only_and_is_adjustable() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(first_reply_secs(&c, "local"), 90);
        assert_eq!(first_reply_secs(&c, "anthropic"), 0);
        assert_eq!(set_first_reply(&c, "glm", 120).unwrap()["glm"], 120);
        set_first_reply(&c, "local", 0).unwrap();
        assert_eq!(first_reply_secs(&c, "local"), 0, "kapatılabilir");
        assert_eq!(first_reply_secs(&c, "glm"), 120, "diğer kayıt korunur");
        assert!(set_first_reply(&c, "local", 5).is_err());
        assert!(set_first_reply(&c, "bogus", 60).is_err());
    }

    #[test]
    fn limits_default_roundtrip_and_validation() {
        let c = crate::db::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(limits(&c), default_limits()); // satır yok → 80/0/0
        let l = Limits { total_cpu_percent: 60, agent_cpu_percent: 25, mem_bytes: 6 << 30 };
        assert_eq!(set_limits(&c, l).unwrap().stored, l);
        assert_eq!(limits(&c), l); // yeniden okuma = kaydedilen
        assert!(set_limits(&c, Limits { total_cpu_percent: 101, ..l }).is_err());
        assert!(set_limits(&c, Limits { mem_bytes: 1024, ..l }).is_err());
        assert_eq!(limits(&c), l, "reddedilen yazım kaydı değiştirmez");
        c.execute("UPDATE settings SET value = 'bozuk' WHERE key = 'limits'", []).unwrap();
        assert_eq!(limits(&c), default_limits(), "okunamayan satır → varsayılan");
    }
}
