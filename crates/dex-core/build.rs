// #2442: folder skills. Each tree in TREES (`skills/<tree>/**`) is embedded whole into the binary
// as `(relative path, include_str!)` pairs (`$OUT_DIR/skill_trees.rs`, read by src/skills.rs), so the
// server build carries it too. No new crate: `include_dir` cannot tell stable cargo about a changed
// or added file, a build script can — `rerun-if-changed` on a directory scans the whole tree.
// Text only (include_str!): a non-UTF-8 file fails the build instead of shipping garbled.

const TREES: &[&str] = &["gamedev"];

fn main() {
    use std::fmt::Write;
    use std::path::{Path, PathBuf};

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.unwrap().path();
            if path.is_dir() { walk(&path, out) } else { out.push(path) }
        }
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut src = String::from("pub const TREES: &[(&str, &[(&str, &str)])] = &[\n");
    for tree in TREES {
        let root = manifest.join("skills").join(tree);
        println!("cargo:rerun-if-changed={}", root.display());
        let mut files = Vec::new();
        walk(&root, &mut files);
        let mut files: Vec<(String, PathBuf)> = files
            .into_iter()
            .map(|p| (p.strip_prefix(&root).unwrap().components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"), p))
            .collect();
        files.sort();
        writeln!(src, "    ({tree:?}, &[").unwrap();
        for (rel, path) in files {
            writeln!(src, "        ({rel:?}, include_str!({:?})),", path.to_string_lossy()).unwrap();
        }
        src.push_str("    ]),\n");
    }
    src.push_str("];\n");
    std::fs::write(PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("skill_trees.rs"), src).unwrap();
}
