//! Persistent PTY session contract shared by WebSocket connections.

use dex_core::pty::{Attachment, PtyInfo};
use dex_core::{Core, PtySink};

#[derive(Default)]
pub struct Sessions;

impl Sessions {
    pub fn spawned(&self, _id: &str) {}

    pub fn list(&self, core: &Core) -> Vec<PtyInfo> {
        core.ptys.list()
    }

    pub fn attach(&self, core: &Core, id: &str, sink: impl PtySink) -> Result<Attachment, String> {
        core.ptys.attach(id, sink)
    }

    pub fn detach(&self, core: &Core, id: &str, subscriber: Option<u64>) {
        if let Some(subscriber) = subscriber {
            core.ptys.detach(id, subscriber);
        }
    }
}
