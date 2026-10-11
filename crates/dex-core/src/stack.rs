//! #322 — the project's stack profile. Which commands test/build a project comes from the repo, not
//! from a central technology list: `<repo root>/.dex/stack.md` is read at spawn and goes into every
//! agent's system prompt (role text → `## Proje yığını` → Dex rules). No profile → Dex looks at the
//! marker files in the repo root and only the leader gets a note to write one with the patron.
//! Dex never writes the file. Read at session start; an edit applies from the next session.

use rusqlite::Connection;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const FILE: &str = ".dex/stack.md";
/// Above this the profile is cut (on a char boundary) and the leader is warned.
pub const LIMIT: usize = 4096;

/// Prompt-time marker inventory: no symlinks, heavy directories or unbounded recursion/reads.
const MARKER_DEPTH: usize = 3;
const MARKER_ENTRIES: usize = 1024;
const MARKER_FILE_LIMIT: u64 = 64 * 1024;

pub(crate) struct TestMarkers {
    root: PathBuf,
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl TestMarkers {
    pub(crate) fn scan(root: &Path) -> Self {
        let mut out = Self { root: root.to_path_buf(), files: Vec::new(), dirs: Vec::new() };
        let mut pending = vec![(PathBuf::new(), 0)];
        let mut remaining = MARKER_ENTRIES;
        while let Some((relative, depth)) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(root.join(&relative)) else { continue };
            for entry in entries {
                if remaining == 0 {
                    return out;
                }
                remaining -= 1;
                let Ok(entry) = entry else { continue };
                let Ok(kind) = entry.file_type() else { continue };
                let name = entry.file_name();
                let path = relative.join(&name);
                if kind.is_file() {
                    out.files.push(path);
                } else if kind.is_dir() && !matches!(name.to_str(), Some(
                    ".git" | ".dex" | "node_modules" | "target" | "build" | "dist" | ".gradle" |
                    ".godot" | "Library" | "Temp" | "Pods" | ".dart_tool"
                )) {
                    out.dirs.push(path.clone());
                    if depth + 1 < MARKER_DEPTH {
                        pending.push((path, depth + 1));
                    }
                }
            }
        }
        out
    }

    pub(crate) fn file(&self, path: &str) -> bool {
        self.files.iter().any(|p| p == Path::new(path))
    }

    pub(crate) fn dir(&self, path: &str) -> bool {
        self.dirs.iter().any(|p| p == Path::new(path))
    }

    pub(crate) fn root_config(&self, prefix: &str) -> bool {
        self.root_file_matching(|name| name.starts_with(prefix))
    }

    pub(crate) fn root_file_matching(&self, matches: impl Fn(&str) -> bool) -> bool {
        self.files.iter().any(|p| p.parent() == Some(Path::new("")) && matches(&p.to_string_lossy()))
    }

    pub(crate) fn dir_named(&self, name: &str) -> bool {
        self.dirs.iter().any(|p| p.file_name().is_some_and(|n| n == name))
    }

    pub(crate) fn text(&self, path: &str) -> Option<String> {
        if !self.file(path) {
            return None;
        }
        let f = std::fs::File::open(self.root.join(path)).ok()?;
        let mut bytes = Vec::new();
        f.take(MARKER_FILE_LIMIT + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MARKER_FILE_LIMIT {
            return None;
        }
        String::from_utf8(bytes).ok()
    }
}

/// Fixed draft template the leader fills in.
const TEMPLATE: &str = "motor/dil, proje yapısı, test komutu, derleme/çalıştırma, kod kuralları, rollerin bu yığındaki karşılığı";

pub struct Profile {
    pub text: String,
    /// Original size in bytes when it was over `LIMIT`.
    pub cut_from: Option<usize>,
}

/// `<cwd>/.dex/stack.md`, cut to `LIMIT`; None when missing or empty.
pub fn read(cwd: &Path) -> Option<Profile> {
    let f = std::fs::File::open(cwd.join(FILE)).ok()?;
    let size = f.metadata().ok()?.len();
    read_capped(f, size)
}

/// Reads at most `LIMIT + 1` bytes (#339: a huge file must not be loaded — spawn holds the DB
/// lock); `size` is the original size reported in the leader note.
fn read_capped(r: impl Read, size: u64) -> Option<Profile> {
    let mut bytes = Vec::new();
    r.take(LIMIT as u64 + 1).read_to_end(&mut bytes).ok()?;
    let mut cut = bytes.len() > LIMIT;
    if cut {
        bytes.truncate(LIMIT);
        // Drop a char split by the cut; earlier invalid bytes are left to the lossy decode.
        if let Err(e) = std::str::from_utf8(&bytes) {
            if e.error_len().is_none() {
                bytes.truncate(e.valid_up_to());
            }
        }
    }
    let mut all = String::from_utf8_lossy(&bytes).into_owned();
    // #351: an invalid byte decodes to U+FFFD (3 bytes), so the text can outgrow LIMIT again.
    if all.len() > LIMIT {
        let mut end = LIMIT;
        while !all.is_char_boundary(end) {
            end -= 1;
        }
        all.truncate(end);
        cut = true;
    }
    let all = if cut { all.trim_end() } else { all.trim() };
    if all.is_empty() {
        return None;
    }
    Some(Profile { text: all.trim_start().to_string(), cut_from: cut.then_some(size as usize) })
}

/// Stacks recognised from marker files in the repo root (Tauri also under `src-tauri/`).
pub fn detect(cwd: &Path) -> Vec<&'static str> {
    let has = |p: &str| cwd.join(p).exists();
    let names: Vec<String> = std::fs::read_dir(cwd)
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    let any = |f: &dyn Fn(&str) -> bool| names.iter().any(|n| f(n));
    let tauri_conf = any(&|n| n.starts_with("tauri.conf"))
        || std::fs::read_dir(cwd.join("src-tauri"))
            .map(|d| d.filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with("tauri.conf")))
            .unwrap_or(false);
    let cargo = has("Cargo.toml") || has("src-tauri/Cargo.toml");
    let mut out = Vec::new();
    if has("project.godot") {
        out.push("Godot");
    }
    if any(&|n| n.ends_with(".uproject")) {
        out.push("Unreal");
    }
    if cwd.join("ProjectSettings").is_dir() {
        out.push("Unity");
    }
    if cargo && tauri_conf {
        out.push("Tauri");
    } else if cargo {
        out.push("Rust");
    }
    if any(&|n| n.starts_with("build.gradle")) {
        out.push("Android");
    }
    if has("package.json") {
        out.push("Node/web");
    }
    if has("pyproject.toml") {
        out.push("Python");
    }
    out
}

/// The `## Proje yığını` section for every agent of the team.
pub fn section(p: &Profile) -> String {
    format!("## Proje yığını\n{}", p.text)
}

/// Leader-only note at the end of the rules: profile missing (with what was detected) or cut.
pub fn leader_note(cwd: &Path, p: Option<&Profile>) -> Option<String> {
    match p {
        Some(Profile { cut_from: Some(n), .. }) => Some(format!(
            "## Proje yığını notu\n`{FILE}` {n} bayt; ilk {LIMIT} baytı eklendi, gerisi kesildi. Patronla birlikte kısalt."
        )),
        Some(_) => None,
        None => {
            let found = detect(cwd);
            let found = if found.is_empty() { "yok".to_string() } else { found.join(", ") };
            Some(format!(
                "## Proje yığını notu\nBu projede `{FILE}` yok; tespit edilen: {found}. Patronla birlikte taslağı yaz \
                 (başlıklar: {TEMPLATE}); dosyayı sen yazarsın, Dex yazmaz, sonraki oturumda devreye girer. \
                 Takip edilsin istiyorsan .gitignore'da `.dex/` yerine `.dex/*` yaz ve \
                 altına `!{FILE}` ekle (dışlanmış dizinin içindeki dosya geri alınamaz)."
            ))
        }
    }
}

/// Legacy automatic single-project root; multi-project and service contexts require an explicit binding.
pub fn team_cwd(conn: &Connection, agent: &str) -> Option<PathBuf> {
    let team: String = conn.query_row("SELECT team_id FROM agents WHERE id=?1", [agent], |r| r.get(0)).ok()?;
    // DB-only resolver: data_dir affects administrative/memory fallbacks, never project_root.
    // Legacy wrapper has no trusted job binding; only a single project is automatic.
    crate::project_context::resolve(
        conn,
        Path::new(""),
        crate::project_context::ResolveRequest {
            team_id: &team,
            agent_id: None,
            delegation_id: None,
            project_id: None,
            mode: crate::project_context::ContextMode::Team,
        },
    )
    .ok()?
    .project_root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marker_inventory_and_reads_are_bounded() {
        let f = crate::projects::tests::Fixture::new();
        std::fs::write(f.0.join("build.gradle"), "a".repeat(MARKER_FILE_LIMIT as usize + 1)).unwrap();
        let markers = TestMarkers::scan(&f.0);
        assert!(markers.text("build.gradle").is_none());
        std::fs::write(f.0.join("build.gradle"), "connectedAndroidTest").unwrap();
        assert_eq!(markers.text("build.gradle").as_deref(), Some("connectedAndroidTest"));
        assert!(markers.text("../Cargo.toml").is_none());
        for i in 0..MARKER_ENTRIES + 10 {
            std::fs::write(f.0.join(format!("file-{i}")), "").unwrap();
        }
        let markers = TestMarkers::scan(&f.0);
        assert_eq!(markers.files.len(), MARKER_ENTRIES);
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dex-stack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detects_markers() {
        let g = tmp("godot");
        std::fs::write(g.join("project.godot"), "").unwrap();
        assert_eq!(detect(&g), vec!["Godot"]);
        let t = tmp("tauri");
        std::fs::create_dir_all(t.join("src-tauri")).unwrap();
        std::fs::write(t.join("src-tauri/tauri.conf.json"), "{}").unwrap();
        std::fs::write(t.join("Cargo.toml"), "").unwrap();
        std::fs::write(t.join("package.json"), "{}").unwrap();
        assert_eq!(detect(&t), vec!["Tauri", "Node/web"]);
        let o = tmp("other");
        std::fs::create_dir_all(o.join("ProjectSettings")).unwrap();
        for f in ["X.uproject", "build.gradle.kts", "pyproject.toml", "Cargo.toml"] {
            std::fs::write(o.join(f), "").unwrap();
        }
        assert_eq!(detect(&o), vec!["Unreal", "Unity", "Rust", "Android", "Python"]);
        assert!(detect(&tmp("empty")).is_empty());
    }

    #[test]
    fn cuts_on_char_boundary_over_limit() {
        let d = tmp("big");
        std::fs::create_dir_all(d.join(".dex")).unwrap();
        std::fs::write(d.join(FILE), format!("a{}", "ğ".repeat(LIMIT))).unwrap(); // ğ = 2 bytes: LIMIT falls mid-char
        let p = read(&d).unwrap();
        assert_eq!((p.text.len(), p.cut_from), (LIMIT - 1, Some(LIMIT * 2 + 1)));
        let n = leader_note(&d, Some(&p)).unwrap();
        assert!(n.contains(&format!("{} bayt; ilk {LIMIT} baytı eklendi", LIMIT * 2 + 1)), "{n}");
        std::fs::write(d.join(FILE), "kısa").unwrap();
        let p = read(&d).unwrap();
        assert!(p.cut_from.is_none() && leader_note(&d, Some(&p)).is_none());
        std::fs::write(d.join(FILE), "  \n").unwrap();
        assert!(read(&d).is_none());
    }

    #[test]
    fn reads_only_up_to_limit() {
        // An endless reader: an unbounded read would never return.
        let p = read_capped(std::io::repeat(b'a'), u64::MAX).unwrap();
        assert_eq!((p.text.len(), p.cut_from), (LIMIT, Some(u64::MAX as usize)));
    }

    #[test]
    fn lossy_decode_stays_within_limit() {
        // Each 0xFF becomes U+FFFD (3 bytes): 4096 bytes in → 12 KB decoded unless cut again.
        let p = read_capped(&[0xFFu8; LIMIT][..], LIMIT as u64).unwrap();
        assert!(p.text.len() <= LIMIT, "{}", p.text.len());
        assert_eq!(p.cut_from, Some(LIMIT));
        assert!(leader_note(&tmp("lossy"), Some(&p)).is_some());
    }
}
