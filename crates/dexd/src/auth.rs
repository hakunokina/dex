//! Device pairing. `dexd pair` (a separate process, same data dir) stores a one-time code; a
//! client trades it once for a long-lived device token. Both live in `<data_dir>/dexd.db` only as
//! SHA-256 hashes — tokens are 256 random bits, so a plain hash is enough (no password KDF), and a
//! stolen DB gives neither a usable token nor a code. Revoking deletes the device row; open
//! connections re-check their device periodically (the CLI can't reach into the server process).

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// No 0/O/1/I/L: the code is read off one screen and typed on another.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 8;
pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub last_seen: Option<i64>,
}

pub struct Auth(Mutex<Connection>);

impl Auth {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            dex_core::private_fs::create_dir(dir).map_err(|e| e.to_string())?;
        }
        dex_core::private_fs::sqlite(path).map_err(|e| e.to_string())?;
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        // The CLI and the server write the same file.
        conn.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS pair_codes (hash TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS devices (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE,
                 created_at INTEGER NOT NULL, last_seen INTEGER);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self(Mutex::new(conn)))
    }

    /// A fresh one-time code, shown as `XXXX-XXXX`; valid for `ttl`, single use.
    pub fn new_pair_code(&self, ttl: Duration) -> Result<String, String> {
        let raw = random_code()?;
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM pair_codes WHERE expires_at <= ?1", [now()]).map_err(|e| e.to_string())?;
        conn.execute("INSERT INTO pair_codes (hash, expires_at) VALUES (?1, ?2)", params![hash(&raw), now() + ttl.as_secs() as i64])
            .map_err(|e| e.to_string())?;
        Ok(format!("{}-{}", &raw[..4], &raw[4..]))
    }

    /// Consumes `code` (whether or not it was typed with the dash / in lower case) → the new
    /// device and its token. The token is returned exactly once and never stored in clear.
    pub fn pair(&self, code: &str, name: &str) -> Result<(Device, String), String> {
        let code: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
        let name = name.trim();
        let name = if name.is_empty() { "cihaz" } else { name };
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let used = tx
            .execute("DELETE FROM pair_codes WHERE hash = ?1 AND expires_at > ?2", params![hash(&code), now()])
            .map_err(|e| e.to_string())?;
        if used == 0 {
            return Err("eşleme kodu geçersiz ya da süresi dolmuş".into());
        }
        let token = hex(&random::<32>()?);
        let device = Device { id: hex(&random::<4>()?), name: name.chars().take(64).collect(), created_at: now(), last_seen: None };
        tx.execute(
            "INSERT INTO devices (id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![device.id, device.name, hash(&token), device.created_at],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok((device, token))
    }

    /// The device holding `token`, or `None` (unknown / revoked). Marks it seen.
    pub fn verify(&self, token: &str) -> Option<Device> {
        let conn = self.0.lock().unwrap();
        let d = conn
            .query_row(
                "SELECT id, name, created_at, last_seen FROM devices WHERE token_hash = ?1",
                [hash(token)],
                |r| Ok(Device { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)?, last_seen: r.get(3)? }),
            )
            .optional()
            .ok()
            .flatten()?;
        let _ = conn.execute("UPDATE devices SET last_seen = ?1 WHERE id = ?2", params![now(), d.id]);
        Some(d)
    }

    /// Still paired (not revoked since the connection authenticated).
    pub fn exists(&self, device_id: &str) -> bool {
        let conn = self.0.lock().unwrap();
        // A DB error keeps the connection: a busy file must not drop every client.
        conn.query_row("SELECT 1 FROM devices WHERE id = ?1", [device_id], |_| Ok(())).optional().map_or(true, |r| r.is_some())
    }

    pub fn list(&self) -> Result<Vec<Device>, String> {
        let conn = self.0.lock().unwrap();
        let mut st = conn.prepare("SELECT id, name, created_at, last_seen FROM devices ORDER BY created_at").map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok(Device { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)?, last_seen: r.get(3)? }))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// `false` = no such device.
    pub fn revoke(&self, device_id: &str) -> Result<bool, String> {
        let conn = self.0.lock().unwrap();
        Ok(conn.execute("DELETE FROM devices WHERE id = ?1", [device_id]).map_err(|e| e.to_string())? > 0)
    }
}

/// #146 (A0): the desktop's own loopback endpoint. One random 256-bit token per process, handed
/// to its webview in memory only — never a `devices` row, never on disk. Kept as a SHA-256 digest
/// and compared in constant time (digests: the length of a guess tells nothing either).
pub struct LocalToken([u8; 32]);

impl LocalToken {
    /// The verifier and the token (the latter goes to the webview only).
    pub fn new() -> Result<(Self, String), String> {
        let token = hex(&random::<32>()?);
        Ok((Self(Sha256::digest(token.as_bytes()).into()), token))
    }

    pub fn verify(&self, token: &str) -> bool {
        let guess: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        guess.iter().zip(self.0).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

fn random_code() -> Result<String, String> {
    // Rejection sampling keeps every letter equally likely (256 % 31 != 0).
    let mut out = String::with_capacity(CODE_LEN);
    while out.len() < CODE_LEN {
        for b in random::<16>()? {
            if (b as usize) < 256 - 256 % ALPHABET.len() && out.len() < CODE_LEN {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    Ok(out)
}

fn hash(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    #[test]
    fn memory_auth_does_not_require_a_directory() {
        let auth = Auth::open(Path::new(":memory:")).unwrap();
        assert_eq!(auth.new_pair_code(CODE_TTL).unwrap().len(), 9);
    }
    #[cfg(unix)]
    #[test]
    fn auth_repairs_existing_directory_database_and_sidecar_modes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("private-auth-{}-{}", std::process::id(), hex(&random::<4>().unwrap())));
        let path = dir.join("dexd.db");
        let first = Auth::open(&path).unwrap();
        let code = first.new_pair_code(CODE_TTL).unwrap();
        let (_, token) = first.pair(&code, "fixture").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let files = [path.clone(), dir.join("dexd.db-wal"), dir.join("dexd.db-shm")];
        for file in &files { std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap(); }
        for _ in 0..2 {
            let second = Auth::open(&path).unwrap();
            assert!(second.verify(&token).is_some());
            assert_eq!(second.list().unwrap().len(), 1);
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            for file in &files { assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600); }
        }
        drop(first);
        std::fs::remove_dir_all(dir).unwrap();
    }
    use super::*;

    fn auth() -> (Auth, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dexd-auth-{}-{}", std::process::id(), hex(&random::<4>().unwrap())));
        (Auth::open(&dir.join("dexd.db")).unwrap(), dir)
    }

    #[test]
    fn code_pairs_once_and_token_verifies() {
        let (a, dir) = auth();
        let code = a.new_pair_code(CODE_TTL).unwrap();
        assert_eq!(code.len(), 9);
        assert!(code.chars().all(|c| c == '-' || ALPHABET.contains(&(c as u8))), "{code}");
        // Typed without the dash, in lower case.
        let (dev, token) = a.pair(&code.replace('-', "").to_lowercase(), " laptop ").unwrap();
        assert_eq!(dev.name, "laptop");
        assert_eq!(token.len(), 64);
        assert!(a.pair(&code, "again").is_err(), "a code is single-use");
        assert_eq!(a.verify(&token).unwrap().id, dev.id);
        assert!(a.verify("nope").is_none());
        // Stored hashed only: neither the token nor the code appears in the file.
        drop(a);
        let raw = std::fs::read(dir.join("dexd.db")).unwrap();
        let wal = std::fs::read(dir.join("dexd.db-wal")).unwrap_or_default();
        for blob in [&raw, &wal] {
            let text = String::from_utf8_lossy(blob);
            assert!(!text.contains(&token) && !text.contains(&code.replace('-', "")));
        }
    }

    #[test]
    fn expired_code_and_revoked_device_are_refused() {
        let (a, _) = auth();
        let code = a.new_pair_code(Duration::ZERO).unwrap();
        assert!(a.pair(&code, "x").is_err());
        let (dev, token) = a.pair(&a.new_pair_code(CODE_TTL).unwrap(), "x").unwrap();
        assert!(a.exists(&dev.id));
        assert_eq!(a.list().unwrap().len(), 1);
        assert!(a.revoke(&dev.id).unwrap());
        assert!(!a.revoke(&dev.id).unwrap());
        assert!(a.verify(&token).is_none());
        assert!(!a.exists(&dev.id));
    }

    #[test]
    fn local_token_verifies_only_itself() {
        let (local, token) = LocalToken::new().unwrap();
        assert_eq!(token.len(), 64);
        assert!(local.verify(&token));
        assert!(!local.verify(&token[..63]));
        assert!(!local.verify(""));
        let (_, other) = LocalToken::new().unwrap();
        assert_ne!(token, other, "random per process");
        assert!(!local.verify(&other));
    }
}
