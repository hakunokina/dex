//! Plan #99 mapgen: proje başına `.dex/map.md` dosyasının "İskelet" bölümünü üreten
//! çekirdek özelliği (Aider repo-map yaklaşımı, kişiselleştirme YOK).
//!
//! tree-sitter ile kaynak dosyalar ayrıştırılır (Rust, TypeScript/TSX, JavaScript/JSX;
//! Kotlin yok), her dosyada tanımlanan ve başvurulan semboller çıkarılır. Dosya grafiği
//! (A, B'de tanımlı bir sembolü kullanıyorsa A→B) PageRank ile sıralanır; [`TOKEN_BUDGET`]
//! içindeki en önemli dosyaların imza listesi + dizin ağacı yazılır.
//!
//! Çıktı deterministiktir (aynı ağaç → aynı dosya): gezinti sıralı, haritalar BTree,
//! zaman damgası/commit yok. Scout-2'nin "Genel bakış" bölümü
//! (`<!-- dex-map:overview:begin -->` … `<!-- dex-map:overview:end -->`) bayt bayt korunur;
//! iskelet kendi işaretleri arasında, genel bakışın ALTINA yazılır. Dosya yoksa yalnız
//! iskeletle oluşturulur.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Token bütçesi — tek yer. ~4 karakter = 1 token sayılır.
pub const TOKEN_BUDGET: usize = 6000;
/// Proje köküne göre harita dosyası. `.dex/` git dışındadır (`.dex/*`).
pub const MAP_FILE: &str = ".dex/map.md";

const OVERVIEW_BEGIN: &str = "<!-- dex-map:overview:begin -->";
const OVERVIEW_END: &str = "<!-- dex-map:overview:end -->";
const SKELETON_BEGIN: &str = "<!-- dex-map:skeleton:begin -->";
const SKELETON_END: &str = "<!-- dex-map:skeleton:end -->";

/// Ayrıştırılan kaynak uzantılar. Kotlin bilerek yok (plan #99: istenirse eklenir).
fn language_for(extension: &str) -> Option<tree_sitter::Language> {
    match extension {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "ts" | "mts" | "cts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "js" | "mjs" | "cjs" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        _ => None,
    }
}

/// Taranmayan dizin adları: vendored/üretilmiş/derleme çıktısı.
fn skipped_dir(name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    matches!(
        name,
        "node_modules" | "target" | "gen" | "dist" | "build" | "vendor" | "third_party" | ".venv" | "__pycache__" | ".gradle" | ".godot" | "Library" | "Temp" | "Pods" | ".dart_tool" | "out" | ".next" | ".nuxt" | "coverage"
    )
}

/// 512 KiB üstü ve üretilmiş işaretli dosyalar taranmaz (bundle/gen).
fn skipped_file(path: &Path, len: u64) -> bool {
    if len > 512 * 1024 {
        return true;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".min.js") || name.ends_with(".bundle.js") {
        return true;
    }
    let Ok(head) = std::fs::read(path) else { return false };
    let head = &head[..head.len().min(512)];
    let head = String::from_utf8_lossy(head);
    head.contains("DO NOT EDIT") || head.contains("auto-generated") || head.contains("@generated")
}

struct Def {
    kind: &'static str,
    name: String,
    sig: String,
    line: usize,
}

struct FileInfo {
    /// Kökten göreli, `/` ayrıcıyla (deterministik).
    rel: String,
    defs: Vec<Def>,
    /// Başvurulan sembol adları (sıralı, tekrarsız).
    refs: Vec<String>,
    unparsed: bool,
}

/// Tanım düğüm türleri: (rust, ts/js ortak küme değil — dil başına ayrı).
fn rust_def_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_item" => Some("fn"),
        "function_signature_item" => Some("fn"),
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "union_item" => Some("union"),
        "trait_item" => Some("trait"),
        "mod_item" => Some("mod"),
        "const_item" => Some("const"),
        "static_item" => Some("static"),
        "type_item" => Some("type"),
        "macro_definition" => Some("macro"),
        _ => None,
    }
}

fn ts_def_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" | "generator_function_declaration" | "function" => Some("fn"),
        "class_declaration" | "abstract_class_declaration" => Some("class"),
        "interface_declaration" => Some("interface"),
        "type_alias_declaration" => Some("type"),
        "enum_declaration" => Some("enum"),
        "namespace_declaration" => Some("namespace"),
        "variable_declarator" => Some("const"),
        "method_definition" | "method_signature" => Some("method"),
        _ => None,
    }
}

fn ref_kind(kind: &str) -> bool {
    matches!(kind, "identifier" | "type_identifier" | "property_identifier")
}

fn parse_file(root: &Path, rel: &str) -> FileInfo {
    let empty = || FileInfo { rel: rel.to_string(), defs: Vec::new(), refs: Vec::new(), unparsed: true };
    let path = root.join(rel);
    let Ok(meta) = std::fs::metadata(&path) else { return empty() };
    if skipped_file(&path, meta.len()) {
        return empty();
    }
    let Ok(src) = std::fs::read_to_string(&path) else { return empty() };
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let Some(lang) = language_for(ext) else { return empty() };
    let is_rust = ext == "rs";
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&lang).is_err() {
        return empty();
    }
    let Some(tree) = parser.parse(&src, None) else { return empty() };
    if tree.root_node().has_error() && tree.root_node().child_count() == 0 {
        return empty();
    }
    let bytes = src.as_bytes();
    let lines: Vec<&str> = src.split('\n').collect();
    let sig_of = |node: tree_sitter::Node| {
        let line = lines.get(node.start_position().row).unwrap_or(&"").trim();
        const MAX: usize = 120;
        if line.chars().count() > MAX {
            let mut end = 0;
            for (i, (b, _)) in line.char_indices().enumerate() {
                if i >= MAX {
                    break;
                }
                end = b + line[b..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
            }
            format!("{}…", &line[..end])
        } else {
            line.to_string()
        }
    };
    // 1. tur: tanımlar (+ tanım adı düğüm id'leri, 2. turda başvuru sayılmasın).
    let mut defs = Vec::new();
    let mut name_ids = HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let kind_fn = if is_rust { rust_def_kind } else { ts_def_kind };
        if let Some(kind) = kind_fn(node.kind()) {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(bytes) {
                    let name = name.to_string();
                    if !name.is_empty() {
                        name_ids.insert(name_node.id());
                        defs.push(Def {
                            kind,
                            name,
                            sig: sig_of(node),
                            line: node.start_position().row + 1,
                        });
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    // 2. tur: başvurular.
    let mut refs = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.child_count() == 0 {
            if ref_kind(node.kind()) && !name_ids.contains(&node.id()) {
                if let Ok(text) = node.utf8_text(bytes) {
                    if !text.is_empty() {
                        refs.insert(text.to_string());
                    }
                }
            }
        } else {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack.push(child);
            }
        }
    }
    defs.sort_by(|a, b| (a.line, &a.name).cmp(&(b.line, &b.name)));
    FileInfo { rel: rel.to_string(), defs, refs: refs.into_iter().collect(), unparsed: false }
}

/// Kök altındaki kaynak dosyaları (sıralı, göreli `/` yollu).
fn source_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&dir)) else { continue };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        for entry in entries {
            let Ok(kind) = entry.file_type() else { continue };
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let rel = if dir.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                dir.join(name)
            };
            if kind.is_dir() {
                if !skipped_dir(name) {
                    dirs.push(rel);
                }
            } else if kind.is_file() {
                let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
                if language_for(ext).is_some() {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    out
}

/// Dosya grafiğinde PageRank (sönüm 0.85, 25 tur). Girdi sırası = sıralı yol sırası.
fn pagerank(files: &[FileInfo]) -> Vec<f64> {
    let n = files.len();
    if n == 0 {
        return Vec::new();
    }
    let mut symbol_to_files: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, f) in files.iter().enumerate() {
        for d in &f.defs {
            // Tek-iki harfli adlar (`id`, `e`, `t`) her yerde çarpışır — grafiğe girmez.
            if d.name.chars().count() >= 3 {
                symbol_to_files.entry(d.name.as_str()).or_default().push(i);
            }
        }
    }
    // Kenar ağırlığı 1/(tanımlayan dosya sayısı): her dosyada olan boilerplate
    // (`hot`, `Props`) az, tek yerde tanımlı çekirdek sembol çok taşır.
    let mut out_edges: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
    for (i, f) in files.iter().enumerate() {
        for r in &f.refs {
            if r.chars().count() < 3 {
                continue;
            }
            if let Some(targets) = symbol_to_files.get(r.as_str()) {
                let w = 1.0 / targets.len() as f64;
                for &t in targets {
                    if t != i {
                        *out_edges[i].entry(t).or_default() += w;
                    }
                }
            }
        }
    }
    const DAMPING: f64 = 0.85;
    const ITERS: usize = 25;
    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..ITERS {
        let mut next = vec![(1.0 - DAMPING) / n as f64; n];
        let mut dangling = 0.0;
        for (j, edges) in out_edges.iter().enumerate() {
            let total: f64 = edges.values().sum();
            if total <= 0.0 {
                dangling += rank[j];
            } else {
                for (&t, &w) in edges {
                    next[t] += DAMPING * rank[j] * w / total;
                }
            }
        }
        let bonus = DAMPING * dangling / n as f64;
        for v in &mut next {
            *v += bonus;
        }
        rank = next;
    }
    rank
}

fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn dir_summary(files: &[FileInfo]) -> String {
    let mut dirs: BTreeMap<&str, usize> = BTreeMap::new();
    for f in files {
        // Yalnız gerçek üst dizinler — dosyanın kendisi dizin sayılmaz.
        let mut dir = f.rel.as_str();
        let mut has_parent = false;
        while let Some((parent, _)) = dir.rsplit_once('/') {
            *dirs.entry(parent).or_default() += 1;
            dir = parent;
            has_parent = true;
        }
        if !has_parent {
            *dirs.entry(".").or_default() += 1;
        }
    }
    // Kök (".") girdisini at, dizinleri derinliğe göre yaz.
    let mut rows: Vec<(&str, usize)> = dirs.into_iter().filter(|(d, _)| *d != ".").collect();
    rows.sort_by(|a, b| (a.0.matches('/').count(), a.0).cmp(&(b.0.matches('/').count(), b.0)));
    const MAX_DIRS: usize = 80;
    let mut out = String::new();
    for (d, n) in rows.iter().take(MAX_DIRS) {
        out.push_str(&format!("- `{}/` — {n} dosya\n", d));
    }
    if rows.len() > MAX_DIRS {
        out.push_str(&format!("- … {} dizin sığmadı\n", rows.len() - MAX_DIRS));
    }
    out
}

/// İskelet bölümünü üretir (işaretler dahil).
pub fn generate(root: &Path) -> Result<String, String> {
    build(root, TOKEN_BUDGET).map(|(skeleton, _, _)| skeleton)
}

#[cfg(test)]
fn generate_with(root: &Path, budget: usize) -> Result<String, String> {
    build(root, budget).map(|(skeleton, _, _)| skeleton)
}

fn build(root: &Path, budget: usize) -> Result<(String, usize, usize), String> {
    if !root.is_dir() {
        return Err(format!("kök dizin yok: {}", root.display()));
    }
    let paths = source_files(root);
    let files: Vec<FileInfo> = paths.iter().map(|p| parse_file(root, p)).collect();
    let ranks = pagerank(&files);
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|&a, &b| {
        ranks[b]
            .partial_cmp(&ranks[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| files[a].rel.cmp(&files[b].rel))
    });
    let symbols: usize = files.iter().map(|f| f.defs.len()).sum();
    let mut body = String::new();
    body.push_str(&format!(
        "## İskelet\n> Otomatik üretim (`map_refresh` ya da birleştirme sonrası tazelenir). Elle değiştirme — yeniden üretimde kaybolur.\n> {} dosya, {} simge, bütçe ~{} token.\n\n### Dizinler\n{}",
        files.len(),
        symbols,
        budget,
        dir_summary(&files)
    ));
    body.push_str("\n### Dosyalar (önemli sırayla)\n");
    // Kapanış notu payı: yol listesi sığmazsa bile sayı satırı mutlaka yer bulur.
    const NOTE_RESERVE: usize = 40;
    let mut used = estimate_tokens(&body) + NOTE_RESERVE;
    // İmza gösterimi dosya başına sınırlı: bütçe az dosyaya değil, çok dosyaya gitsin.
    const MAX_DEFS_PER_FILE: usize = 25;
    let mut bare: Vec<&str> = Vec::new();
    for &i in &order {
        let f = &files[i];
        let mut section = format!("#### `{}`\n", f.rel);
        if f.unparsed {
            section.push_str("- *(ayrıştırılamadı — yalnız dizin/yol listesinde)*\n");
        }
        let mut shown = 0;
        for d in f.defs.iter().take(MAX_DEFS_PER_FILE) {
            let line = format!("- {} `{}` — {}\n", d.kind, d.name, d.sig);
            if used + estimate_tokens(&section) + estimate_tokens(&line) > budget {
                break;
            }
            section.push_str(&line);
            shown += 1;
        }
        if shown < f.defs.len().min(MAX_DEFS_PER_FILE) || f.defs.len() > MAX_DEFS_PER_FILE {
            let hidden = f.defs.len() - shown;
            let line = format!("- … {hidden} imza\n");
            if used + estimate_tokens(&section) + estimate_tokens(&line) <= budget {
                section.push_str(&line);
            }
        }
        if used + estimate_tokens(&section) > budget {
            bare.push(f.rel.as_str());
            continue;
        }
        used += estimate_tokens(&section);
        body.push_str(&section);
    }
    // Kalan dosyalar yalnız yol olarak — hepsi keşfedilebilir kalsın.
    // Kapanış notu (yol listesi ya da sayı) pay ayrıldığı için her zaman yazılır.
    if !bare.is_empty() {
        let head = "\n### Diğer dosyalar (yalnız yol)\n";
        body.push_str(head);
        used += estimate_tokens(head);
        let mut unlisted = 0;
        for rel in &bare {
            let line = format!("- `{rel}`\n");
            if used + estimate_tokens(&line) > budget {
                unlisted += 1;
                continue;
            }
            body.push_str(&line);
            used += estimate_tokens(&line);
        }
        if unlisted > 0 {
            body.push_str(&format!("- … {unlisted} dosya yolu da sığmadı\n"));
        }
    }
    Ok((format!("{SKELETON_BEGIN}\n{body}{SKELETON_END}\n"), files.len(), symbols))
}

pub struct RefreshOut {
    pub path: PathBuf,
    pub files: usize,
    pub symbols: usize,
    pub bytes: u64,
}

/// Haritayı tazeler: overview bloğu bayt bayt korunur, iskelet altta yenilenir.
/// Dosya yoksa yalnız iskeletle oluşturulur.
pub fn refresh(root: &Path) -> Result<RefreshOut, String> {
    let (skeleton, files, symbols) = build(root, TOKEN_BUDGET)?;
    let map_path = root.join(MAP_FILE);
    let overview = std::fs::read_to_string(&map_path).ok().and_then(|old| {
        let start = old.find(OVERVIEW_BEGIN)?;
        let end = old.find(OVERVIEW_END)?;
        if end < start {
            return None;
        }
        Some(old[start..end + OVERVIEW_END.len()].to_string())
    });
    let mut out = String::new();
    if let Some(overview) = overview {
        out.push_str(&overview);
        out.push_str("\n\n");
    }
    out.push_str(&skeleton);
    if let Some(parent) = map_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&map_path, &out).map_err(|e| e.to_string())?;
    Ok(RefreshOut { path: map_path, files, symbols, bytes: out.len() as u64 })
}

/// Ajan istemindeki tek satır — harita dosyası varsa. Yoksa None (istem baytı değişmez).
pub fn prompt_line(root: &Path) -> Option<String> {
    if root.join(MAP_FILE).is_file() {
        Some(prompt_text(&root.display().to_string()))
    } else {
        None
    }
}

/// [`prompt_line`]'s text for `root`; the prompt-part list (Plan #114) shows it with a placeholder.
pub(crate) fn prompt_text(root: &str) -> String {
    format!("Proje haritası: `{root}/.dex/map.md` — iş karışık gelirse ya da nereden başlayacağını bilmiyorsan bak; hemen bakmak zorunda değilsin, işin ortasında da açabilirsin.")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fx {
        dir: PathBuf,
    }

    impl Fx {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "dex-repomap-{name}-{}-{}",
                std::process::id(),
                crate::agents::next_seq()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn file(&self, rel: &str, body: &str) -> &Self {
            let p = self.dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
            self
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// a: foo'yu tanımlar, bar'ı kullanır. b: bar'ı tanımlar. c: baz'ı tanımlar, foo+bar kullanır.
    /// Kenarlar a→b, c→a, c→b ⇒ sıra b > a > c olmalı.
    fn rank_fixture() -> Fx {
        let f = Fx::new("rank");
        f.file("a.rs", "pub fn foo() {}\npub fn uses() { bar(); }\n");
        f.file("b.rs", "pub fn bar() {}\n");
        f.file("c.rs", "pub fn baz() { foo(); bar(); }\n");
        f
    }

    #[test]
    fn pagerank_orders_by_incoming_references() {
        let f = rank_fixture();
        let paths = source_files(&f.dir);
        assert_eq!(paths, vec!["a.rs", "b.rs", "c.rs"]);
        let files: Vec<FileInfo> = paths.iter().map(|p| parse_file(&f.dir, p)).collect();
        let ranks = pagerank(&files);
        assert!(ranks[1] > ranks[0] && ranks[0] > ranks[2], "{ranks:?}");
    }

    #[test]
    fn rust_and_ts_definitions_are_extracted() {
        let f = Fx::new("defs");
        f.file(
            "lib.rs",
            "pub struct Store;\npub trait Repo {}\npub const LIMIT: usize = 8;\nimpl Store {\n    pub fn get(&self) {}\n}\n",
        );
        f.file(
            "ui.tsx",
            "export interface Props {}\nexport function render(p: Props) {}\nexport class View {}\nconst helper = () => {};\n",
        );
        let info = parse_file(&f.dir, "lib.rs");
        for name in ["Store", "get", "LIMIT"] {
            assert!(info.defs.iter().any(|d| d.name == name), "{name}: {:?}", info.defs.iter().map(|d| &d.name).collect::<Vec<_>>());
        }
        let tsx = parse_file(&f.dir, "ui.tsx");
        for name in ["Props", "render", "View", "helper"] {
            assert!(tsx.defs.iter().any(|d| d.name == name), "{name}: {:?}", tsx.defs.iter().map(|d| &d.name).collect::<Vec<_>>());
        }
        assert!(tsx.refs.contains(&"Props".to_string()));
    }

    #[test]
    fn vendored_and_generated_files_are_excluded() {
        let f = Fx::new("excl");
        f.file("src/main.rs", "pub fn main() {}\n");
        f.file("node_modules/dep/index.js", "function dep() {}\n");
        f.file("target/x.rs", "pub fn x() {}\n");
        f.file(".dex/tmp/y.rs", "pub fn y() {}\n");
        f.file("gen/z.ts", "export function z() {}\n");
        let big = "x".repeat(600 * 1024);
        f.file("big.rs", &format!("pub fn big() {{}}\n// {big}"));
        f.file("g.rs", "// auto-generated DO NOT EDIT\npub fn g() {}\n");
        let paths = source_files(&f.dir);
        assert_eq!(paths, vec!["big.rs", "g.rs", "src/main.rs"], "{paths:?}");
        // big.rs (boy) ve g.rs (üretilmiş işareti) ayrıştırılmaz ama dizinde sayılır.
        assert!(parse_file(&f.dir, "big.rs").unparsed);
        assert!(parse_file(&f.dir, "g.rs").unparsed);
    }

    #[test]
    fn budget_truncates_and_output_is_deterministic() {
        let f = rank_fixture();
        let first = generate_with(&f.dir, TOKEN_BUDGET).unwrap();
        let second = generate_with(&f.dir, TOKEN_BUDGET).unwrap();
        assert_eq!(first, second);
        assert!(first.contains(SKELETON_BEGIN) && first.contains(SKELETON_END));
        assert!(estimate_tokens(&first) <= TOKEN_BUDGET + 500, "{}", estimate_tokens(&first));
        // Kalabalık repo + dar bütçe: imza ve dosya kesintisi olur, çıktı yine deterministik.
        let big = Fx::new("big");
        for i in 0..30 {
            big.file(
                &format!("m{i}.rs"),
                "pub struct Item;\nimpl Item {\n    pub fn new() {}\n    pub fn run(&self) {}\n    pub fn stop(&self) {}\n}\n",
            );
        }
        let tiny = generate_with(&big.dir, 400).unwrap();
        assert!(tiny.contains("Diğer dosyalar") || tiny.contains("sığmadı"), "{tiny}");
        assert!(estimate_tokens(&tiny) <= 400 + 500, "{}", estimate_tokens(&tiny));
        assert_eq!(tiny, generate_with(&big.dir, 400).unwrap());
    }

    #[test]
    fn refresh_preserves_overview_byte_for_byte() {
        let f = rank_fixture();
        let overview = format!("{OVERVIEW_BEGIN}\n## Genel bakış\nScout-2 metni — Türkçe karakterler: ğşç.\n{OVERVIEW_END}\n");
        std::fs::create_dir_all(f.dir.join(".dex")).unwrap();
        std::fs::write(f.dir.join(MAP_FILE), format!("{overview}\n{SKELETON_BEGIN}\neski iskelet\n{SKELETON_END}\n")).unwrap();
        let before = std::fs::read(f.dir.join(MAP_FILE)).unwrap();
        refresh(&f.dir).unwrap();
        let after = std::fs::read_to_string(f.dir.join(MAP_FILE)).unwrap();
        assert!(after.starts_with(&overview), "{after}");
        assert!(!after.contains("eski iskelet"));
        // İkinci üretim overview'u değiştirmez.
        refresh(&f.dir).unwrap();
        let again = std::fs::read(f.dir.join(MAP_FILE)).unwrap();
        let end = |b: &[u8]| b.windows(OVERVIEW_END.len()).position(|w| w == OVERVIEW_END.as_bytes()).unwrap() + OVERVIEW_END.len();
        assert_eq!(&again[..end(&again)], &before[..end(&before)]);
    }

    #[test]
    fn refresh_creates_skeleton_only_when_missing() {
        let f = rank_fixture();
        let out = refresh(&f.dir).unwrap();
        assert_eq!(out.files, 3);
        let text = std::fs::read_to_string(&out.path).unwrap();
        assert!(!text.contains(OVERVIEW_BEGIN));
        assert!(text.contains("#### `b.rs`"));
        assert!(out.bytes == text.len() as u64);
    }

    #[test]
    fn prompt_line_only_when_map_exists() {
        let f = rank_fixture();
        assert!(prompt_line(&f.dir).is_none());
        refresh(&f.dir).unwrap();
        let line = prompt_line(&f.dir).unwrap();
        assert!(line.starts_with("Proje haritası: `") && line.ends_with(".dex/map.md` — iş karışık gelirse ya da nereden başlayacağını bilmiyorsan bak; hemen bakmak zorunda değilsin, işin ortasında da açabilirsin."), "{line}");
    }
}
