//! Desktop storage selection shared by the shell and local-core tooling. Never migrates files.

use std::path::{Path, PathBuf};

pub const IDENTIFIER: &str = "com.hakunokina.dex";
// Accepted compatibility exception: existing desktop installations keep using this root in place.
pub const LEGACY_IDENTIFIER: &str = "com.arda.dex";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoragePaths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub local_data_dir: PathBuf,
}

pub fn resolve(identifier: &str) -> Result<StoragePaths, &'static str> {
    Ok(select_at(
        identifier,
        &dirs::data_dir().ok_or("veri dizini bulunamadı")?,
        &dirs::config_dir().ok_or("config dizini bulunamadı")?,
        &dirs::data_local_dir().ok_or("yerel veri dizini bulunamadı")?,
    ))
}

/// Decide once from the data root, then use the same identity for config and WebView storage.
/// Explicit base directories keep tests independent of Windows Known Folder APIs.
pub fn select_at(
    identifier: &str,
    data_base: &Path,
    config_base: &Path,
    local_base: &Path,
) -> StoragePaths {
    let legacy_data = data_base.join(LEGACY_IDENTIFIER);
    let selected = if identifier == IDENTIFIER
        && legacy_data.is_dir()
        && legacy_data.join("dex.db").is_file()
        && std::fs::File::open(legacy_data.join("dex.db")).is_ok()
    {
        LEGACY_IDENTIFIER
    } else {
        identifier
    };
    StoragePaths {
        data_dir: data_base.join(selected),
        config_dir: config_base.join(selected),
        local_data_dir: local_base.join(selected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|p| p.join("src-tauri").is_dir())
            .unwrap()
            .join(".dex/tmp")
            .join(format!(
                "desktop-storage-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn select(root: &Path, identifier: &str) -> StoragePaths {
        select_at(
            identifier,
            &root.join("data"),
            &root.join("config"),
            &root.join("local"),
        )
    }

    #[test]
    fn complete_legacy_root_selects_all_paths_without_filesystem_changes() {
        let root = fixture("legacy");
        let old = root.join("data").join(LEGACY_IDENTIFIER);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("dex.db"), b"existing database").unwrap();
        // A populated new root does not override a complete legacy installation.
        let new = root.join("data").join(IDENTIFIER);
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("dex.db"), b"new database").unwrap();
        let expected = StoragePaths {
            data_dir: old.clone(),
            config_dir: root.join("config").join(LEGACY_IDENTIFIER),
            local_data_dir: root.join("local").join(LEGACY_IDENTIFIER),
        };
        for _ in 0..2 {
            assert_eq!(select(&root, IDENTIFIER), expected);
        }
        assert_eq!(
            std::fs::read(old.join("dex.db")).unwrap(),
            b"existing database"
        );
        assert_eq!(std::fs::read(new.join("dex.db")).unwrap(), b"new database");
        assert!(!root.join("config").exists());
        assert!(!root.join("local").exists());
        assert_eq!(std::fs::read_dir(&old).unwrap().count(), 1);
    }

    #[test]
    fn absent_legacy_root_selects_new_paths_without_creating_them() {
        let root = fixture("new");
        let paths = select(&root, IDENTIFIER);
        assert_eq!(paths.data_dir, root.join("data").join(IDENTIFIER));
        assert_eq!(paths.config_dir, root.join("config").join(IDENTIFIER));
        assert_eq!(paths.local_data_dir, root.join("local").join(IDENTIFIER));
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    }

    #[test]
    fn running_legacy_core_does_not_change_storage_selection() {
        let root = fixture("locked");
        let old = root.join("data").join(LEGACY_IDENTIFIER);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("dex.db"), b"existing database").unwrap();
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(old.join("dex.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        assert_eq!(select(&root, IDENTIFIER).data_dir, old);
        assert!(!root.join("data").join(IDENTIFIER).exists());
    }

    #[test]
    fn legacy_folder_without_database_does_not_select_legacy_config_or_profile() {
        let root = fixture("incomplete");
        for base in ["data", "config", "local"] {
            std::fs::create_dir_all(root.join(base).join(LEGACY_IDENTIFIER)).unwrap();
        }
        let paths = select(&root, IDENTIFIER);
        assert_eq!(paths.data_dir, root.join("data").join(IDENTIFIER));
        assert_eq!(paths.config_dir, root.join("config").join(IDENTIFIER));
        assert_eq!(paths.local_data_dir, root.join("local").join(IDENTIFIER));
    }

    #[test]
    fn directory_named_dex_db_is_not_a_usable_database() {
        let root = fixture("db-directory");
        std::fs::create_dir_all(root.join("data").join(LEGACY_IDENTIFIER).join("dex.db")).unwrap();
        assert_eq!(
            select(&root, IDENTIFIER).data_dir,
            root.join("data").join(IDENTIFIER)
        );
    }

    #[test]
    fn qa_identity_never_selects_legacy_user_storage() {
        let root = fixture("qa");
        let old = root.join("data").join(LEGACY_IDENTIFIER);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("dex.db"), b"existing database").unwrap();
        let qa = format!("{IDENTIFIER}.qa");
        let paths = select(&root, &qa);
        assert_eq!(paths.data_dir, root.join("data").join(&qa));
        assert_eq!(paths.config_dir, root.join("config").join(&qa));
        assert_eq!(paths.local_data_dir, root.join("local").join(&qa));
    }
}
