//! `dexd`: the Dex core without a UI, served over one WebSocket endpoint (docs/protocol.md).
//! Two ways to run it: `dexd serve` — a host other machines pair with (S4: a tailnet host) — and
//! #150 `dexd --local` — the desktop app's own core in its own process (`local`).

pub mod auth;
pub mod fs;
pub mod local;
pub mod local_auth;
pub mod methods;
pub mod protocol;
pub mod server;
mod sessions;
pub mod tailnet;

pub use server::{serve, Server};

pub const DEFAULT_PORT: u16 = 7717;
