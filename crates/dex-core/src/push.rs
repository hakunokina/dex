//! #1718: content-free FCM HTTP v1 ping for a stored inbox reply (`{type, id}` data only; the
//! phone fetches the reply over its authenticated host connection).
//!
//! Config: the patron's Firebase service-account JSON at `<data_dir>/fcm-service-account.json`
//! (never in the repo; too large for the Windows credential store's 2560-byte blob). Missing →
//! push is off and replies are still stored. Network: Google OAuth and FCM only. Device tokens
//! are secrets: DB only, never logged.
use crate::db::with;
use crate::inbox::Error;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;

pub const SERVICE_ACCOUNT_FILE: &str = "fcm-service-account.json";
const OAUTH_URL: &str = "https://oauth2.googleapis.com/token";
const FCM_URL: &str = "https://fcm.googleapis.com";
const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const JWT_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const MAX_TOKENS: i64 = 20;

#[derive(Deserialize)]
struct ServiceAccount { project_id: String, client_email: String, private_key: String }

pub(crate) struct Push {
    /// (OAuth token URL, FCM base) — fixed outside tests.
    endpoints: Mutex<(String, String)>,
    /// (client_email, access token, expiry unix s): one OAuth exchange per hour.
    cached: Mutex<Option<(String, String, u64)>>,
    /// Sends run one at a time so they share the cached token.
    gate: Mutex<()>,
}

impl Default for Push {
    fn default() -> Self {
        Self { endpoints: Mutex::new((OAUTH_URL.into(), FCM_URL.into())), cached: Mutex::default(), gate: Mutex::default() }
    }
}

#[derive(Debug, PartialEq)]
enum Sent { Ok, Unregistered, Failed(u16) }

fn unix_s() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) }

fn http() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(5)).timeout(Duration::from_secs(15)).redirects(0).build()
}

fn pkcs8(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem.lines().map(str::trim).filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD.decode(body).map_err(|_| "servis hesabı anahtarı PEM değil".to_string())
}

/// RS256 JWT bearer assertion (RFC 7523) for Google's token endpoint.
fn assertion(sa: &ServiceAccount, aud: &str, now: u64) -> Result<String, String> {
    use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let claims = json!({ "iss": sa.client_email, "scope": SCOPE, "aud": aud, "iat": now, "exp": now + 3600 });
    let input = format!("{}.{}", b64.encode(r#"{"alg":"RS256","typ":"JWT"}"#), b64.encode(claims.to_string()));
    let key = RsaKeyPair::from_pkcs8(&pkcs8(&sa.private_key)?).map_err(|_| "servis hesabı anahtarı okunamadı".to_string())?;
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), input.as_bytes(), &mut sig).map_err(|_| "JWT imzalanamadı".to_string())?;
    Ok(format!("{input}.{}", b64.encode(sig)))
}

fn valid_token(token: &str) -> bool {
    (1..=4096).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

impl Push {
    fn access_token(&self, sa: &ServiceAccount) -> Result<String, Sent> {
        let now = unix_s();
        if let Some((email, token, exp)) = self.cached.lock().unwrap().as_ref() {
            if *email == sa.client_email && *exp > now + 60 { return Ok(token.clone()); }
        }
        let url = self.endpoints.lock().unwrap().0.clone();
        let jwt = assertion(sa, &url, now).map_err(|_| Sent::Failed(0))?;
        let reply: Value = match http().post(&url).send_form(&[("grant_type", JWT_GRANT), ("assertion", &jwt)]) {
            Ok(r) => r.into_json().map_err(|_| Sent::Failed(200))?,
            Err(ureq::Error::Status(code, _)) => return Err(Sent::Failed(code)),
            Err(_) => return Err(Sent::Failed(0)),
        };
        let token = reply["access_token"].as_str().filter(|t| !t.is_empty()).ok_or(Sent::Failed(200))?.to_string();
        let exp = now + reply["expires_in"].as_u64().unwrap_or(3600);
        *self.cached.lock().unwrap() = Some((sa.client_email.clone(), token.clone(), exp));
        Ok(token)
    }

    fn send(&self, sa: &ServiceAccount, device: &str, reply: i64) -> Sent {
        let access = match self.access_token(sa) { Ok(t) => t, Err(e) => return e };
        let url = format!("{}/v1/projects/{}/messages:send", self.endpoints.lock().unwrap().1, sa.project_id);
        // Data only: no notification title/body ever leaves the host.
        let body = json!({ "message": { "token": device, "data": { "type": "inbox_reply", "id": reply.to_string() }, "android": { "priority": "HIGH" } } });
        match http().post(&url).set("Authorization", &format!("Bearer {access}")).send_json(body) {
            Ok(_) => Sent::Ok,
            Err(ureq::Error::Status(code, r)) => {
                let error: Value = r.into_json().unwrap_or(Value::Null);
                let unregistered = error["error"]["details"].as_array().is_some_and(|d| d.iter().any(|x| x["errorCode"] == "UNREGISTERED"));
                if code == 401 { self.cached.lock().unwrap().take(); }
                // A bare 404 (wrong project) must not wipe every device: only FCM's verdict does.
                if unregistered { Sent::Unregistered } else { Sent::Failed(code) }
            }
            Err(_) => Sent::Failed(0),
        }
    }
}

impl crate::Core {
    fn push_account(&self) -> Option<ServiceAccount> {
        let text = match std::fs::read_to_string(self.paths.data_dir.join(SERVICE_ACCOUNT_FILE)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => { log::warn!("push: servis hesabı dosyası okunamadı"); return None; }
        };
        // project_id goes into the URL path: nothing that could leave `/v1/projects/<id>`.
        match serde_json::from_str::<ServiceAccount>(&text) {
            Ok(sa) if !sa.project_id.is_empty() && sa.project_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') => Some(sa),
            _ => { log::warn!("push: servis hesabı dosyası geçersiz"); None }
        }
    }

    pub fn push_configured(&self) -> bool { self.push_account().is_some() }

    /// The one status line at start.
    pub(crate) fn push_status_line(&self) {
        if !self.push_configured() {
            eprintln!("dex: push yapılandırılmadı ({} yok)", self.paths.data_dir.join(SERVICE_ACCOUNT_FILE).display());
        }
    }

    pub fn push_register(&self, token: &str, platform: &str) -> Result<Value, Error> {
        if platform != "android" { return Err(Error { code: "push_platform", message: "yalnız android".into() }); }
        if !valid_token(token) { return Err(Error { code: "push_token", message: "geçersiz cihaz jetonu".into() }); }
        let now = crate::diagnostics::now_ms() as i64;
        with(&self.db, |c| {
            c.execute("INSERT INTO push_tokens(token,platform,created_at,updated_at) VALUES(?1,?2,?3,?3)
                ON CONFLICT(token) DO UPDATE SET platform=excluded.platform,updated_at=excluded.updated_at", rusqlite::params![token, platform, now]).map_err(|e| e.to_string())?;
            c.execute("DELETE FROM push_tokens WHERE token NOT IN (SELECT token FROM push_tokens ORDER BY updated_at DESC,created_at DESC LIMIT ?1)", [MAX_TOKENS]).map(|_| ()).map_err(|e| e.to_string())
        }).map_err(|_| Error { code: "push_unavailable", message: "cihaz kaydedilemedi".into() })?;
        Ok(json!({ "ok": true, "pushConfigured": self.push_configured() }))
    }

    pub fn push_unregister(&self, token: &str) -> Result<Value, Error> {
        if !valid_token(token) { return Err(Error { code: "push_token", message: "geçersiz cihaz jetonu".into() }); }
        with(&self.db, |c| c.execute("DELETE FROM push_tokens WHERE token=?1", [token]).map(|_| ()).map_err(|e| e.to_string()))
            .map_err(|_| Error { code: "push_unavailable", message: "cihaz silinemedi".into() })?;
        Ok(json!({ "ok": true }))
    }

    /// Off the hook thread: a slow Google never delays the Stop answer.
    pub(crate) fn push_notify(&self, reply: i64) {
        let weak = self.weak_self.clone();
        std::thread::spawn(move || if let Some(core) = weak.upgrade() { core.push_send_all(reply); });
    }

    /// Returns how many devices accepted the ping.
    fn push_send_all(&self, reply: i64) -> usize {
        let Ok(_gate) = self.push.gate.lock() else { return 0 };
        let Some(sa) = self.push_account() else { return 0 };
        let Ok(tokens) = with(&self.db, |c| {
            let mut stmt = c.prepare("SELECT token FROM push_tokens ORDER BY updated_at DESC").map_err(|e| e.to_string())?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        }) else { return 0 };
        let mut sent = 0;
        for token in tokens {
            match self.push.send(&sa, &token, reply) {
                Sent::Ok => sent += 1,
                Sent::Unregistered => {
                    let _ = with(&self.db, |c| c.execute("DELETE FROM push_tokens WHERE token=?1", [&token]).map(|_| ()).map_err(|e| e.to_string()));
                    log::info!("push: kayıtsız cihaz jetonu silindi");
                }
                Sent::Failed(code) => log::warn!("push: gönderim başarısız (HTTP {code})"),
            }
        }
        sent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::Arc;

    /// A fresh PKCS#8 RSA key from the OpenSSL that ships with Git (no key in the repo).
    fn test_key(dir: &std::path::Path) -> String {
        let out = dir.join("key.pem");
        let mut candidates = vec![std::path::PathBuf::from("openssl")];
        if let Ok(o) = std::process::Command::new("git").arg("--exec-path").output() {
            // <git>/mingw64/libexec/git-core → <git>/mingw64/bin, <git>/usr/bin
            let exec = std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            if let Some(mingw) = exec.parent().and_then(|p| p.parent()) {
                candidates.push(mingw.join("bin/openssl.exe"));
                if let Some(git) = mingw.parent() { candidates.push(git.join("usr/bin/openssl.exe")); }
            }
        }
        let made = candidates.iter().any(|bin| std::process::Command::new(bin)
            .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out"]).arg(&out)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .status().is_ok_and(|s| s.success()));
        assert!(made, "openssl gerekli (test anahtarı üretimi)");
        std::fs::read_to_string(out).unwrap()
    }

    #[derive(Default)]
    struct Seen(Mutex<Vec<(String, String, String)>>);

    /// Fake Google: `/token` checks the RS256 assertion, `messages:send` answers per device.
    fn fake(public: Vec<u8>, seen: Arc<Seen>) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || for mut s in l.incoming().flatten() {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let (head, body) = loop {
                let k = s.read(&mut chunk).unwrap_or(0);
                if k == 0 { break (String::new(), String::new()) }
                buf.extend_from_slice(&chunk[..k]);
                let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..p]).to_string();
                let len = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                if buf.len() >= p + 4 + len { break (head, String::from_utf8_lossy(&buf[p + 4..p + 4 + len]).to_string()) }
            };
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            let auth = head.lines().find_map(|l| l.strip_prefix("Authorization: ").or_else(|| l.strip_prefix("authorization: "))).unwrap_or("").to_string();
            let (status, reply) = if path == "/token" {
                let jwt = body.split("assertion=").nth(1).unwrap_or("");
                let parts: Vec<&str> = jwt.split('.').collect();
                let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
                let ok = parts.len() == 3 && ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, &public)
                    .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &b64.decode(parts[2]).unwrap_or_default()).is_ok();
                let claims: Value = serde_json::from_slice(&b64.decode(parts.get(1).unwrap_or(&"")).unwrap_or_default()).unwrap_or(Value::Null);
                if ok && claims["scope"] == SCOPE && claims["iss"] == "dex-fixture-service-account" && body.contains("grant_type=urn%3Aietf") {
                    ("200 OK", json!({"access_token":"at-1","expires_in":3600,"token_type":"Bearer"}).to_string())
                } else { ("400 Bad Request", "{}".into()) }
            } else if auth != "Bearer at-1" {
                ("401 Unauthorized", "{}".into())
            } else if body.contains("\"dev-gone\"") {
                ("404 Not Found", json!({"error":{"code":404,"status":"NOT_FOUND","details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}).to_string())
            } else if body.contains("\"dev-bad\"") {
                ("404 Not Found", json!({"error":{"code":404,"status":"NOT_FOUND"}}).to_string())
            } else { ("200 OK", json!({"name":"projects/fixture/messages/1"}).to_string()) };
            seen.0.lock().unwrap().push((path, auth, body));
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
        });
        base
    }

    #[test]
    fn push_sends_data_only_with_cached_token_and_drops_unregistered_devices() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("push-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        assert!(!core.push_configured());
        assert_eq!(core.push_register("dev-ok", "android").unwrap()["pushConfigured"], false);
        assert_eq!(core.push_send_all(1), 0, "unconfigured push is silent");
        let pem = test_key(&root);
        let public = ring::signature::RsaKeyPair::from_pkcs8(&pkcs8(&pem).unwrap()).unwrap();
        let public = ring::signature::KeyPair::public_key(&public).as_ref().to_vec();
        let seen = Arc::new(Seen::default());
        let base = fake(public, seen.clone());
        *core.push.endpoints.lock().unwrap() = (format!("{base}/token"), base.clone());
        std::fs::write(core.paths.data_dir.join(SERVICE_ACCOUNT_FILE), json!({
            "type":"service_account","project_id":"fixture","client_email":"dex-fixture-service-account","private_key":pem,
            "token_uri":"https://evil.example/token"
        }).to_string()).unwrap();
        for t in ["dev-gone", "dev-bad"] { core.push_register(t, "android").unwrap(); }
        assert_eq!(core.push_register("dev-ok", "android").unwrap()["pushConfigured"], true);
        assert_eq!(core.push_send_all(7), 1);
        assert_eq!(core.push_send_all(8), 1);
        let tokens: Vec<String> = with(&core.db, |c| {
            let mut s = c.prepare("SELECT token FROM push_tokens ORDER BY token").map_err(|e| e.to_string())?;
            let r = s.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string()); r
        }).unwrap();
        assert_eq!(tokens, ["dev-bad", "dev-ok"], "only FCM's UNREGISTERED removes a device");
        let seen = seen.0.lock().unwrap();
        assert_eq!(seen.iter().filter(|(p, _, _)| p == "/token").count(), 1, "access token is cached");
        let sends: Vec<_> = seen.iter().filter(|(p, _, _)| p == "/v1/projects/fixture/messages:send").collect();
        assert_eq!(sends.len(), 5);
        let ok: Value = serde_json::from_str(&sends.iter().find(|(_, _, b)| b.contains("dev-ok")).unwrap().2).unwrap();
        assert_eq!(ok, json!({"message":{"token":"dev-ok","data":{"type":"inbox_reply","id":"7"},"android":{"priority":"HIGH"}}}));
        drop(seen);
        assert_eq!(core.push_register("x", "ios").unwrap_err().code, "push_platform");
        assert_eq!(core.push_register("a b", "android").unwrap_err().code, "push_token");
        core.push_unregister("dev-ok").unwrap();
        core.shutdown();
    }

    #[test]
    fn device_list_is_bounded_and_project_id_cannot_leave_the_fcm_path() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("push-cap-{}-{}", std::process::id(), crate::agents::next_seq()));
        let core = crate::Core::start(crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") }, crate::Events::new(crate::events::NullSink)).unwrap();
        for i in 0..MAX_TOKENS + 5 { core.push_register(&format!("dev-{i}"), "android").unwrap(); }
        let n: i64 = with(&core.db, |c| c.query_row("SELECT COUNT(*) FROM push_tokens", [], |r| r.get(0)).map_err(|e| e.to_string())).unwrap();
        assert_eq!(n, MAX_TOKENS);
        std::fs::write(core.paths.data_dir.join(SERVICE_ACCOUNT_FILE), json!({"project_id":"x/../../y","client_email":"e","private_key":"k"}).to_string()).unwrap();
        assert!(!core.push_configured());
        core.shutdown();
    }
}
