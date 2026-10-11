//! S4: the server's folder picker. A client choosing a remote team's project folder browses the
//! server's disk, but only under the roots `dexd` was started with (`--root`, default
//! `~/projects`); nothing above them is listed, created or accepted as a team folder.
//! Every path is canonicalized first, so `..` and symlinks cannot step out.

use dex_core::teams::normalize_dir;
use serde::Serialize;
use std::path::PathBuf;

pub struct Roots(Vec<PathBuf>);

#[derive(Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct Listing {
    /// `None` = the roots themselves.
    pub path: Option<String>,
    /// `None` at a root (going up leaves the allowed area) and at the root list.
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

impl Roots {
    /// Adds an existing team's workspace to this request only, never to global roots.
    pub fn with_workspace(&self, workspace: Option<&str>) -> Result<Self, String> {
        let mut paths = self.0.clone();
        if let Some(workspace) = workspace {
            if !std::path::Path::new(workspace).try_exists().map_err(|e|e.to_string())? { return Ok(Self(paths)); }
            let root = PathBuf::from(normalize_dir(workspace)?);
            if !paths.contains(&root) { paths.push(root); }
        }
        Ok(Self(paths))
    }
    /// Each root is created when missing (a fresh server has no `~/projects` yet).
    pub fn new(dirs: &[PathBuf]) -> Result<Self, String> {
        let mut roots = Vec::new();
        for d in dirs {
            std::fs::create_dir_all(d).map_err(|e| format!("kök klasör açılamadı {}: {e}", d.display()))?;
            roots.push(PathBuf::from(normalize_dir(&d.to_string_lossy())?));
        }
        if roots.is_empty() {
            return Err("en az bir kök klasör gerekli".into());
        }
        Ok(Self(roots))
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.0
    }

    /// The canonical folder, if it is a root or below one.
    fn resolve(&self, raw: &str) -> Result<PathBuf, String> {
        let p = PathBuf::from(normalize_dir(raw)?);
        if self.0.iter().any(|r| p.starts_with(r)) {
            Ok(p)
        } else {
            Err(format!("{raw} izinli klasörlerin dışında"))
        }
    }

    /// A team folder (`team_create` / `team_update` cwd) must be inside the roots.
    pub fn check(&self, dir: &str) -> Result<(), String> {
        self.resolve(dir).map(|_| ())
    }

    pub fn list(&self, path: Option<&str>) -> Result<Listing, String> {
        let Some(raw) = path.filter(|p| !p.trim().is_empty()) else {
            let entries = self.0.iter().map(|r| Entry { name: r.to_string_lossy().into(), path: r.to_string_lossy().into() }).collect();
            return Ok(Listing { path: None, parent: None, entries });
        };
        let dir = self.resolve(raw)?;
        let mut entries = Vec::new();
        for e in std::fs::read_dir(&dir).map_err(|e| format!("klasör okunamadı: {e}"))?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            // A symlink pointing out of the roots is not shown (and would be refused anyway).
            let Ok(target) = self.resolve(&e.path().to_string_lossy()) else { continue };
            if target.is_dir() {
                entries.push(Entry { name, path: e.path().to_string_lossy().into() });
            }
        }
        entries.sort_by_key(|e| e.name.to_lowercase());
        let parent = if self.0.contains(&dir) { None } else { dir.parent().map(|p| p.to_string_lossy().into()) };
        Ok(Listing { path: Some(dir.to_string_lossy().into()), parent, entries })
    }

    /// One new folder directly under `parent`; returns its path.
    pub fn mkdir(&self, parent: &str, name: &str) -> Result<String, String> {
        let name = name.trim();
        let bad = name.is_empty() || name == "." || name == ".." || name.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'));
        if bad {
            return Err(format!("geçersiz klasör adı: {name}"));
        }
        let dir = self.resolve(parent)?.join(name);
        std::fs::create_dir(&dir).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => format!("{name} zaten var"),
            _ => format!("klasör oluşturulamadı: {e}"),
        })?;
        normalize_dir(&dir.to_string_lossy())
    }
}

/// `~/projects` of the user dexd runs as (on the server: the `dex` user).
pub fn default_root() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("projects")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn team_workspace_allows_mkdir_without_expanding_global_roots() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/fs-1059").join(std::process::id().to_string());
        std::fs::create_dir_all(base.join("team")).unwrap();
        std::fs::create_dir_all(base.join("other")).unwrap();
        let roots = Roots::new(&[base.join("global")]).unwrap();
        let team = normalize_dir(base.join("team").to_str().unwrap()).unwrap();
        let scoped = roots.with_workspace(Some(&team)).unwrap();
        let made = scoped.mkdir(&team,"Sibling").unwrap();
        assert!(Path::new(&made).is_dir());
        assert!(roots.check(&made).is_err());
        assert!(scoped.mkdir(base.join("other").to_str().unwrap(),"Denied").is_err());
        assert!(scoped.check(base.to_str().unwrap()).is_err());
        assert_eq!(scoped.list(Some(&team)).unwrap().parent,None);
        assert!(roots.with_workspace(Some(base.join("legacy-missing").to_str().unwrap())).unwrap().check(base.join("global").to_str().unwrap()).is_ok());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn lists_and_creates_only_under_the_roots() {
        let base = std::env::temp_dir().join(format!("dexd-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("projects");
        std::fs::create_dir_all(root.join("app/src")).unwrap();
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        std::fs::create_dir_all(base.join("secret")).unwrap();
        std::fs::write(root.join("file.txt"), "x").unwrap();
        let roots = Roots::new(std::slice::from_ref(&root)).unwrap();
        let r = roots.paths()[0].to_string_lossy().to_string();

        let top = roots.list(None).unwrap();
        assert_eq!(top.entries.len(), 1);
        assert_eq!(top.entries[0].path, r);

        let l = roots.list(Some(&r)).unwrap();
        assert_eq!(l.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["app"], "dosya ve gizli klasör yok");
        assert_eq!(l.parent, None, "kökten yukarı çıkılmaz");
        let app = roots.list(Some(&l.entries[0].path)).unwrap();
        assert_eq!(app.parent.as_deref(), Some(r.as_str()));

        for out in [base.join("secret"), root.join(".."), root.join("app/../../secret")] {
            assert!(roots.list(Some(&out.to_string_lossy())).is_err(), "{}", out.display());
            assert!(roots.check(&out.to_string_lossy()).is_err());
        }
        assert!(roots.check(&root.join("app").to_string_lossy()).is_ok());

        let made = roots.mkdir(&r, "Yeni Proje").unwrap();
        assert!(Path::new(&made).is_dir());
        assert!(roots.mkdir(&r, "Yeni Proje").unwrap_err().contains("zaten var"));
        for bad in ["..", "a/b", "a\\b", "", "c:x"] {
            assert!(roots.mkdir(&r, bad).is_err(), "{bad}");
        }
        assert!(roots.mkdir(&base.to_string_lossy(), "kaçak").is_err());
        assert!(!base.join("kaçak").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
