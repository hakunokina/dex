//! #41: one Core per data dir. Two processes on the same `dex.db` (release + `tauri dev`, or a
//! `dexd --data-dir` pointed at the app's dir) would each run `reconcile_on_start` and close the
//! other's open delegations. The guard is an exclusive OS lock on `<data_dir>/dex.lock`
//! (`LockFileEx` / `flock`): the OS drops it with the handle, so a crashed process never leaves a
//! stale lock behind. Held for the Core's lifetime; released by [`Lock`]'s drop.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

pub const LOCK_FILE: &str = "dex.lock";

#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] File);

/// Takes the data dir's lock, or explains that another Dex already holds it.
pub fn acquire(data_dir: &Path) -> Result<Lock, String> {
    let path = data_dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Lock(file)),
        Err(TryLockError::WouldBlock) => Err(format!(
            "bu veri dizinini başka bir Dex süreci kullanıyor: {} — aynı dex.db'yi iki süreç açamaz",
            data_dir.display()
        )),
        Err(TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dex-instance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn second_lock_on_same_dir_is_refused_until_first_drops() {
        let d = dir("same");
        let first = acquire(&d).unwrap();
        let err = acquire(&d).unwrap_err();
        assert!(err.contains("başka bir Dex"), "{err}");
        drop(first);
        acquire(&d).unwrap();
    }

    #[test]
    fn different_dirs_lock_independently() {
        let (a, b) = (dir("a"), dir("b"));
        let _la = acquire(&a).unwrap();
        let _lb = acquire(&b).unwrap();
    }

    /// The case that broke #48: a second Core on the same dir must fail before reconcile runs.
    #[test]
    fn second_core_on_same_data_dir_does_not_start() {
        use crate::{events, Core, Events, Paths};
        let root = dir("core");
        let paths = |c: &str| Paths { data_dir: root.join("data"), config_dir: root.join(c) };
        let first = Core::start(paths("config1"), Events::new(events::NullSink)).unwrap();
        let err = Core::start_bare(paths("config2"), Events::new(events::NullSink)).err().unwrap();
        assert!(err.contains("başka bir Dex"), "{err}");
        let other = Core::start(Paths { data_dir: root.join("data2"), config_dir: root.join("config3") }, Events::new(events::NullSink)).unwrap();
        first.shutdown();
        let second = Core::start(paths("config2"), Events::new(events::NullSink)).unwrap();
        second.shutdown();
        other.shutdown();
    }
}
