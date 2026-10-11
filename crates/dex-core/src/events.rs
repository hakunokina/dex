//! App-wide events (`tasks-changed`, `dex-hook`, …) without a UI framework.
//!
//! The core only knows [`EventSink`]; the desktop shell bridges it to `AppHandle::emit`, `dexd`
//! (S2) to its WebSocket clients. Event names and payload shapes are the frontend's contract and
//! do not change with the transport.

use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

/// A leader finished its turn with no outstanding team work. Shared by both hosts.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamTurnDone {
    pub team_id: String,
    pub leader_id: String,
    /// Unix time in milliseconds.
    pub timestamp: u64,
}

pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String>;
}

/// Cheap to clone; every subsystem keeps one.
#[derive(Clone)]
pub struct Events(Arc<dyn EventSink>);

impl Events {
    pub fn new(sink: impl EventSink) -> Self {
        Self(Arc::new(sink))
    }

    pub fn emit<T: Serialize + ?Sized>(&self, event: &str, payload: &T) -> Result<(), String> {
        let v = serde_json::to_value(payload).map_err(|e| e.to_string())?;
        self.0.emit(event, v)
    }
}

/// Drops everything (tests, headless tools).
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _: &str, _: Value) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Rec(Mutex<Vec<(String, Value)>>);
    impl EventSink for Arc<Rec> {
        fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
            self.0.lock().unwrap().push((event.into(), payload));
            Ok(())
        }
    }

    #[test]
    fn unit_payload_serializes_like_tauri() {
        let rec = Arc::new(Rec::default());
        let ev = Events::new(rec.clone());
        ev.emit("tasks-changed", &()).unwrap();
        ev.emit("delegations-changed", &Some(serde_json::json!({"id": 1}))).unwrap();
        let got = rec.0.lock().unwrap();
        // Tauri serializes `()` as JSON null; listeners see the same value.
        assert_eq!(got[0], ("tasks-changed".into(), Value::Null));
        assert_eq!(got[1].1["id"], 1);
    }
}
