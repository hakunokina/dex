//! Storage is selected before core/plugin/WebView startup; all consumers use this state.

use tauri::{AppHandle, Manager, Runtime};

pub use crate::desktop_storage::StoragePaths as AppPaths;

pub fn prepare(config: &mut tauri::Config) -> Result<AppPaths, &'static str> {
    let paths = crate::desktop_storage::resolve(&config.identifier)?;
    Ok(prepare_at(config, paths))
}

fn prepare_at(config: &mut tauri::Config, paths: AppPaths) -> AppPaths {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    for window in &mut config.app.windows {
        window.data_directory = Some(paths.local_data_dir.clone());
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    let _ = config;
    paths
}

pub fn get<R: Runtime>(app: &AppHandle<R>) -> AppPaths {
    app.state::<AppPaths>().inner().clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop_storage::{select_at, IDENTIFIER, LEGACY_IDENTIFIER};
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../.dex/tmp")
            .join(format!(
                "shell-storage-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn prepare_fixture(root: &std::path::Path) -> (tauri::Config, AppPaths) {
        let mut config = tauri::Config {
            identifier: IDENTIFIER.into(),
            ..Default::default()
        };
        let window = tauri::utils::config::WindowConfig {
            data_directory: Some(root.join("wrong-profile")),
            ..Default::default()
        };
        config.app.windows.extend([window, Default::default()]);
        let paths = select_at(
            IDENTIFIER,
            &root.join("data"),
            &root.join("config"),
            &root.join("local"),
        );
        let selected = prepare_at(&mut config, paths);
        assert_eq!(config.identifier, IDENTIFIER);
        for window in &config.app.windows {
            assert_eq!(
                window.data_directory.as_ref(),
                Some(&selected.local_data_dir)
            );
        }
        (config, selected)
    }

    #[test]
    fn complete_legacy_storage_is_used_in_place_without_copying() {
        let root = fixture("legacy");
        let old_data = root.join("data").join(LEGACY_IDENTIFIER);
        let old_config = root.join("config").join(LEGACY_IDENTIFIER);
        let old_local = root.join("local").join(LEGACY_IDENTIFIER);
        for dir in [&old_data, &old_config, &old_local] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(old_data.join("dex.db"), b"database").unwrap();
        std::fs::write(old_config.join("layout.json"), b"existing shell layout").unwrap();
        std::fs::write(old_local.join("webview-storage"), b"hosts-and-settings").unwrap();
        let (_, selected) = prepare_fixture(&root);
        assert_eq!(selected.data_dir, old_data);
        assert_eq!(selected.config_dir, old_config);
        assert_eq!(selected.local_data_dir, old_local);
        assert_eq!(
            std::fs::read(selected.config_dir.join("layout.json")).unwrap(),
            b"existing shell layout"
        );
        assert_eq!(
            std::fs::read(selected.local_data_dir.join("webview-storage")).unwrap(),
            b"hosts-and-settings"
        );
        assert!(!root.join("data").join(IDENTIFIER).exists());
        assert!(!root.join("config").join(IDENTIFIER).exists());
        assert!(!root.join("local").join(IDENTIFIER).exists());
        assert!(!selected.data_dir.join("logs").exists());
    }

    #[test]
    fn absent_legacy_storage_selects_new_paths_for_every_window() {
        let root = fixture("new");
        let (_, selected) = prepare_fixture(&root);
        assert_eq!(selected.data_dir, root.join("data").join(IDENTIFIER));
        assert_eq!(selected.config_dir, root.join("config").join(IDENTIFIER));
        assert_eq!(selected.local_data_dir, root.join("local").join(IDENTIFIER));
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    }

    #[test]
    fn incomplete_legacy_storage_selects_new_paths_for_every_window() {
        let root = fixture("incomplete");
        for base in ["data", "config", "local"] {
            std::fs::create_dir_all(root.join(base).join(LEGACY_IDENTIFIER)).unwrap();
        }
        let (_, selected) = prepare_fixture(&root);
        assert_eq!(selected.data_dir, root.join("data").join(IDENTIFIER));
        assert_eq!(selected.config_dir, root.join("config").join(IDENTIFIER));
        assert_eq!(selected.local_data_dir, root.join("local").join(IDENTIFIER));
    }
}
