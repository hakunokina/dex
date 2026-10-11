//! Wire format (docs/protocol.md). Text frames are JSON: requests `{id, method, params}`, replies
//! `{id, result}` / `{id, error: {code, message}}`, server events `{event, payload}`. PTY bytes
//! travel in binary frames: a big-endian u32 channel id, then the raw bytes — both directions.

use serde::Deserialize;
use serde_json::{json, Value};

/// Bumped on any incompatible change; the client sends it in `auth.hello` / `auth.pair`.
pub const PROTOCOL: u32 = 1;

#[derive(Debug, Deserialize)]
pub struct Request {
    /// Echoed back verbatim (the client's correlation id).
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new("bad_request", message)
    }
    /// The core refused (its error text is meant for the user, as with Tauri's `invoke`).
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new("failed", message)
    }
}

pub fn reply(id: &Value, r: Result<Value, Error>) -> String {
    match r {
        Ok(result) => json!({ "id": id, "result": result }),
        Err(e) => json!({ "id": id, "error": { "code": e.code, "message": e.message } }),
    }
    .to_string()
}

pub fn event(name: &str, payload: &Value) -> String {
    json!({ "event": name, "payload": payload }).to_string()
}

pub fn pty_frame(channel: u32, bytes: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(4 + bytes.len());
    f.extend_from_slice(&channel.to_be_bytes());
    f.extend_from_slice(bytes);
    f
}

pub fn parse_pty_frame(frame: &[u8]) -> Option<(u32, &[u8])> {
    let (head, body) = frame.split_first_chunk::<4>()?;
    Some((u32::from_be_bytes(*head), body))
}

/// One named argument, like Tauri's command args: a missing key is `null` (→ `None` for options).
pub fn arg<T: serde::de::DeserializeOwned>(params: &Value, key: &str) -> Result<T, Error> {
    serde_json::from_value(params.get(key).cloned().unwrap_or(Value::Null)).map_err(|e| Error::bad_request(format!("{key}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_frames_round_trip() {
        let f = pty_frame(0x0102_0304, b"hi");
        assert_eq!(f, [1, 2, 3, 4, b'h', b'i']);
        assert_eq!(parse_pty_frame(&f), Some((0x0102_0304, &b"hi"[..])));
        assert_eq!(parse_pty_frame(&[0, 0, 0, 7]), Some((7, &b""[..])));
        assert_eq!(parse_pty_frame(&[0, 0, 7]), None);
    }

    #[test]
    fn replies_and_events_have_the_documented_shape() {
        let ok: Value = serde_json::from_str(&reply(&json!(3), Ok(json!([1])))).unwrap();
        assert_eq!(ok, json!({"id": 3, "result": [1]}));
        let err: Value = serde_json::from_str(&reply(&json!("a"), Err(Error::failed("yok")))).unwrap();
        assert_eq!(err, json!({"id": "a", "error": {"code": "failed", "message": "yok"}}));
        let ev: Value = serde_json::from_str(&event("tasks-changed", &Value::Null)).unwrap();
        assert_eq!(ev, json!({"event": "tasks-changed", "payload": null}));
    }

    #[test]
    fn requests_parse_with_optional_params() {
        let r: Request = serde_json::from_str(r#"{"id":1,"method":"team_list"}"#).unwrap();
        assert_eq!((r.id, r.method.as_str(), r.params), (json!(1), "team_list", Value::Null));
        let p = json!({"teamId": "t1", "n": 2});
        assert_eq!(arg::<Option<String>>(&p, "teamId").unwrap().as_deref(), Some("t1"));
        assert_eq!(arg::<Option<String>>(&p, "missing").unwrap(), None);
        assert_eq!(arg::<String>(&p, "missing").unwrap_err().code, "bad_request");
        assert_eq!(arg::<u32>(&p, "n").unwrap(), 2);
    }
}
