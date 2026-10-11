//! Usage: maintenance_dry_run <existing dex.db> <repository root> [live agent ids...]
//! Intentionally has no execute option and never starts Core or runs migrations.
use dex_core::maintenance::{dry_run, Reason};
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    time::SystemTime,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let db = args.next().ok_or("existing dex.db required")?;
    let root = PathBuf::from(args.next().ok_or("repository root required")?);
    let live: HashSet<_> = args.collect();
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let now = SystemTime::now();
    let entries = dry_run(&c, &root, now, &live)?;
    let eligible = entries.iter().filter(|e| e.reason == Reason::Ready).count();
    let bytes = entries
        .iter()
        .filter(|e| e.reason == Reason::Ready)
        .fold(0u64, |n, e| n.saturating_add(e.bytes));
    let mut skipped = BTreeMap::<String, u64>::new();
    for e in entries.iter().filter(|e| e.reason != Reason::Ready) {
        let reason = serde_json::to_value(e.reason)?
            .as_str()
            .ok_or("reason")?
            .to_owned();
        *skipped.entry(reason).or_default() += 1;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "capturedAtMs":now.duration_since(SystemTime::UNIX_EPOCH)?.as_millis() as u64,
            "eligible":eligible,"estimatedBytes":bytes,"skipped":skipped,"entries":entries
        }))?
    );
    Ok(())
}
