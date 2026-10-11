//! #41: one Core per data dir. Two processes on the same `dex.db` (release + `tauri dev`, or a
//! `dexd --data-dir` pointed at the app's dir) would each run `reconcile_on_start` and close the
//! other's open delegations. The guard is an exclusive OS lock on `<data_dir>/dex.lock`
//! (`LockFileEx` / `flock`): a crashed process never leaves a stale lock behind once inherited
//! descriptors close. Held for the Core's lifetime; explicitly released by [`Lock`]'s drop on
//! Unix, where a forked child's descriptor could otherwise prolong the lock until exec.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

pub const LOCK_FILE: &str = "dex.lock";

#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] File);

#[cfg(unix)]
impl Drop for Lock {
    fn drop(&mut self) {
        // CLOEXEC prevents inheritance past exec, not during fork -> pre_exec. Unlock the
        // shared open-file description now; close alone may leave the child's reference locked.
        if let Err(e) = self.0.unlock() {
            // No panic during shutdown. File still closes; on failure a retained reference
            // stays locked (fail closed) until the child closes it, rather than allowing overlap.
            eprintln!("dex: instance kilidi bırakılamadı: {e}");
        }
    }
}

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

    /// dup and fork retain the same flock reference. CLOEXEC only closes it at exec,
    /// so shutdown must release the lock even while a child is still in pre_exec.
    #[cfg(unix)]
    #[test]
    fn drop_releases_lock_while_an_inherited_descriptor_lives() {
        use std::os::fd::AsRawFd;
        let d = dir("inherited");
        let first = acquire(&d).unwrap();
        let flags = unsafe { libc::fcntl(first.0.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        let inherited = first.0.try_clone().unwrap();
        assert!(acquire(&d).is_err(), "a live Lock still excludes a second instance");
        drop(first);
        let second = acquire(&d).expect("shutdown releases flock before the child's exec");
        // Closing the old child's descriptor must not release the NEW instance's lock.
        drop(inherited);
        assert!(acquire(&d).is_err());
        drop(second);
        acquire(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn core_shutdown_releases_an_inherited_lock_before_restart() {
        let root = dir("core-inherited");
        let paths = || crate::Paths { data_dir: root.join("data"), config_dir: root.join("config") };
        let first = crate::Core::start(paths(), crate::Events::new(crate::events::NullSink)).unwrap();
        let inherited = first.instance.lock().unwrap().as_ref().unwrap().0.try_clone().unwrap();
        assert!(crate::Core::start_bare(paths(), crate::Events::new(crate::events::NullSink)).is_err());
        first.shutdown();
        let second = crate::Core::start(paths(), crate::Events::new(crate::events::NullSink)).unwrap();
        drop(inherited);
        assert!(crate::Core::start_bare(paths(), crate::Events::new(crate::events::NullSink)).is_err());
        second.shutdown();
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
