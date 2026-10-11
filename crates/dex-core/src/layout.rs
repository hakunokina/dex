//! Pane layout persistence (`<config_dir>/layout.json`).
//!
//! The file's schema belongs to the frontend; Rust only checks that it is a JSON
//! object and stores/loads it atomically (tmp + rename), so later schema changes
//! (DEX-7 multi-team) need no Rust change. A missing or corrupt file loads as
//! `None` and is never deleted — the next save simply overwrites it.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn path_of(config_dir: &Path) -> PathBuf {
    config_dir.join("layout.json")
}

pub fn read_file(path: &Path) -> Result<Option<Value>, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(None) };
    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        // Corrupt or non-object: keep defaults, let the next debounced save heal the file.
        other => {
            eprintln!("layout.json geçersiz, yok sayılıyor: {other:?}");
            Ok(None)
        }
    }
}

pub fn write_file(path: &Path, layout: &Value) -> Result<(), String> {
    if !layout.is_object() {
        return Err("layout bir JSON nesnesi olmalı".into());
    }
    let dir = path.parent().ok_or("layout yolu dizinsiz")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(layout).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dex-layout-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_load_roundtrip() {
        let p = tmpdir("roundtrip").join("layout.json");
        let v: Value = serde_json::from_str(
            r#"{"teams":{"default":{"left":[{"id":"p1","agentId":"timur","sessionId":null}],"right":[],"sizes":{"left":[1],"right":[1,2]}}}}"#,
        )
        .unwrap();
        write_file(&p, &v).unwrap();
        // "App restart": nothing cached, fresh read from disk must equal what was saved.
        assert_eq!(read_file(&p).unwrap(), Some(v));
    }

    #[test]
    fn save_is_idempotent() {
        let p = tmpdir("idempotent").join("layout.json");
        let v: Value = serde_json::json!({"teams": {}});
        write_file(&p, &v).unwrap();
        write_file(&p, &v).unwrap(); // twice — same result, no error
        assert_eq!(read_file(&p).unwrap(), Some(v));
    }

    #[test]
    fn missing_and_corrupt_load_as_none() {
        let d = tmpdir("corrupt");
        assert_eq!(read_file(&d.join("yok.json")).unwrap(), None);
        let p = d.join("bozuk.json");
        std::fs::write(&p, "{yok").unwrap();
        assert_eq!(read_file(&p).unwrap(), None);
        let n = d.join("dizi.json");
        std::fs::write(&n, "[1,2]").unwrap();
        assert_eq!(read_file(&n).unwrap(), None);
    }

    #[test]
    fn rejects_non_object_save() {
        let p = tmpdir("nonobject").join("layout.json");
        let v: Value = serde_json::json!([1, 2]);
        assert!(write_file(&p, &v).is_err());
    }
}
