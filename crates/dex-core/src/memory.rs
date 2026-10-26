//! Kademe 1 "Ajan Hafıza Haritası"nın salt okuma RPC'leri: `memory_graph` + `memory_read`.
//!
//! Legacy APIs show the default project; additive APIs select one project or the team.
//! Explicit memory anchors come from project_context; graph IDs qualify scope and owner.
//! Reads accept only IDs in that view, with canonical containment checks.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn search_tokens(text: &str) -> Vec<String> {
    let normalized: String = text
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| *c != '\u{307}')
        .map(|c| match c {
            'ş' => 's',
            'ğ' => 'g',
            'ü' => 'u',
            'ö' => 'o',
            'ç' => 'c',
            'ı' => 'i',
            _ => c,
        })
        .collect();
    normalized.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// Only the directories visible in this agent's index, never all agents under a root.
pub(crate) struct SearchScope {
    pub own: PathBuf,
    dirs: Vec<SearchDir>,
}

struct SearchDir {
    root: PathBuf,
    dir: PathBuf,
    /// None: shared; Some(None): private in a project-less session.
    project: Option<Option<String>>,
}

impl SearchScope {
    /// Resolve folders under the DB mutex; scan their files after releasing it.
    pub(crate) fn for_context(conn: &rusqlite::Connection, ctx: &crate::project_context::ProjectContext) -> Result<Self, String> {
        let agent = ctx.agent_id.as_deref().ok_or("hafıza için ajan gerekli")?;
        let own = crate::slug::memory_dir(conn, &ctx.agent_memory_root, agent)?;
        let mut dirs = vec![
            SearchDir { root: ctx.team_memory_root.clone(), dir: ctx.team_memory_root.join("shared"), project: None },
            SearchDir { root: ctx.agent_memory_root.clone(), dir: own.clone(), project: Some(ctx.project_id.clone()) },
        ];
        if let Some(root) = ctx.project_memory_root.as_ref().filter(|_| ctx.project_team_id.as_deref().is_none_or(|t| t == ctx.team_id)) {
            let dir = root.join("shared");
            if !dirs.iter().any(|d| same_dir(&d.dir, &dir)) {
                dirs.push(SearchDir { root: root.clone(), dir, project: None });
            }
        }
        Ok(Self { own, dirs })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchHit {
    pub slug: String,
    pub description: String,
    pub path: PathBuf,
    pub snippet: String,
}

struct SearchDoc {
    hit: SearchHit,
    fields: [BTreeMap<String, usize>; 3],
    lengths: [usize; 3],
    modified: std::time::SystemTime,
}

fn search_snippet(text: &str, query: &BTreeSet<String>) -> Option<String> {
    let mut offset = 0;
    let matched = text.split_inclusive(|c: char| !c.is_alphanumeric() && c != '\u{307}').find(|word| {
        if search_tokens(word).iter().any(|t| query.contains(t)) {
            true
        } else {
            offset += word.len();
            false
        }
    });
    matched?;
    let start = text[..offset].chars().count().saturating_sub(80);
    Some(text.chars().skip(start).take(240).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Fresh filesystem corpus, weighted BM25 (name 5, description 3, body 1). No DB/cache to drift.
pub(crate) fn search(scope: &SearchScope, query: &str, limit: usize) -> Vec<SearchHit> {
    let query: BTreeSet<_> = search_tokens(query).into_iter().collect();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut docs = Vec::new();
    for scope in &scope.dirs {
        let Ok(root) = scope.root.canonicalize() else { continue };
        let Ok(dir) = scope.dir.canonicalize() else { continue };
        if !dir.starts_with(&root) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&scope.dir) else { continue };
        for path in files.flatten().map(|f| f.path()) {
            if !path.is_file()
                || !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
                || path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("MEMORY.md"))
                || !inside(&dir, &path)
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if scope.project.as_ref().is_some_and(|p| !visible_in_project(&text, p.as_deref())) {
                continue;
            }
            let slug = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let (fm, body) = split_frontmatter(&text);
            let name = fm.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| slug.clone());
            let description = fm.get("description").cloned().unwrap_or_default();
            let mut lengths = [0; 3];
            let fields = [&name, &description, &body].map(|field| {
                let mut counts = BTreeMap::new();
                for token in search_tokens(field) {
                    *counts.entry(token).or_insert(0) += 1;
                }
                counts
            });
            for (i, counts) in fields.iter().enumerate() {
                lengths[i] = counts.values().sum();
            }
            let snippet = [&body, &description, &name].into_iter().find_map(|s| search_snippet(s, &query)).unwrap_or_default();
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            docs.push(SearchDoc { hit: SearchHit { slug, description, path, snippet }, fields, lengths, modified });
        }
    }
    if docs.is_empty() {
        return Vec::new();
    }
    let n = docs.len() as f64;
    let average: [f64; 3] = std::array::from_fn(|i| (docs.iter().map(|d| d.lengths[i]).sum::<usize>() as f64 / n).max(1.0));
    let idf: BTreeMap<_, _> = query.iter().map(|token| {
        let df = docs.iter().filter(|d| d.fields.iter().any(|f| f.contains_key(token))).count() as f64;
        (token, (1.0 + (n - df + 0.5) / (df + 0.5)).ln())
    }).collect();
    let mut ranked: Vec<_> = docs.into_iter().filter_map(|doc| {
        let mut score = 0.0;
        for (token, idf) in &idf {
            for (i, weight) in [5.0, 3.0, 1.0].into_iter().enumerate() {
                let tf = doc.fields[i].get(*token).copied().unwrap_or(0) as f64;
                score += weight * idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * doc.lengths[i] as f64 / average[i]));
            }
        }
        (score > 0.0).then_some((score, doc))
    }).collect();
    ranked.sort_by(|(a, x), (b, y)| b.total_cmp(a).then_with(|| y.modified.cmp(&x.modified)).then_with(|| x.hit.path.cmp(&y.hit.path)));
    ranked.into_iter().take(limit).map(|(_, d)| d.hit).collect()
}

/// Graph'ın ürettiği tek id biçimi; `..`, yol ayırıcı ve kök dışı her şey reddedilir.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemNode {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub agent_id: Option<String>,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub links: Vec<String>,
    pub updated_at: u64,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemEdge {
    pub from: String,
    pub to: String,
}

#[derive(Serialize)]
pub struct MemGraph {
    pub nodes: Vec<MemNode>,
    pub edges: Vec<MemEdge>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemDoc {
    pub id: String,
    pub frontmatter: BTreeMap<String, String>,
    pub body: String,
}

/// Baştaki `---` bloğu: düz `anahtar: değer` satırları (girintili `metadata:` altındakiler
/// dahil, `.` ile birleşir) + blok sonrası gövde. Blok yoksa/eksikse boş map + tüm metin.
pub(crate) fn split_frontmatter(text: &str) -> (BTreeMap<String, String>, String) {
    let stripped = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = stripped.lines();
    if lines.next().is_none_or(|l| l.trim_end() != "---") {
        return (BTreeMap::new(), text.to_string());
    }
    let (mut fm, mut section) = (BTreeMap::new(), String::new());
    let rest: Vec<&str> = lines.collect();
    let mut body_from = rest.len();
    for (i, line) in rest.iter().enumerate() {
        if line.trim_end() == "---" {
            body_from = i + 1;
            break;
        }
        let indented = line.starts_with([' ', '\t']);
        let Some((k, v)) = line.split_once(':') else { continue };
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        if indented && !section.is_empty() {
            fm.insert(format!("{section}.{k}"), v);
        } else if !indented {
            section = if v.is_empty() { k.to_string() } else { String::new() };
            if !v.is_empty() {
                fm.insert(k.to_string(), v);
            }
        }
    }
    if body_from > rest.len() {
        body_from = rest.len();
    }
    (fm, rest[body_from..].join("\n"))
}

/// Ham `[[slug]]` listesi (görüntü metni `[[slug|etiket]]` ise slug kısmı).
pub(crate) fn links_of(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(s) = rest.find("[[") {
        let after = &rest[s + 2..];
        let Some(e) = after.find("]]") else { break };
        let slug = after[..e].split('|').next().unwrap_or("").trim();
        if !slug.is_empty() && !out.iter().any(|x: &String| x == slug) {
            out.push(slug.to_string());
        }
        rest = &after[e + 2..];
    }
    out
}

struct File {
    /// Graph id'si (`shared/<name>`, `agents/<aid>/<name>`).
    id: String,
    scope: String,
    agent: Option<String>,
    stem: String,
}

/// Kanonik yol `root_canon` içinde mi — kök dışına çıkan symlink/junction'ı eler (#157 F1).
pub(crate) fn inside(root_canon: &Path, p: &Path) -> bool {
    p.canonicalize().is_ok_and(|c| c.starts_with(root_canon))
}

/// Canonical aliases (including junctions) have one scope; legacy shared belongs to the team.
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b)
}

fn list_dir(root_canon: &Path, dir: &Path, scope: &str, agent: Option<&str>) -> Vec<File> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && inside(root_canon, p))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("MEMORY.md")))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_string_lossy().into_owned();
            if stem.is_empty() || stem.contains('/') || stem.contains('\\') {
                return None;
            }
            let id = match agent {
                Some(a) => format!("agents/{a}/{stem}"),
                None => format!("{scope}/{stem}"),
            };
            Some(File { id, scope: scope.into(), agent: agent.map(str::to_string), stem })
        })
        .collect()
}

fn file_meta(root: &Path, f: &File) -> Option<MemNode> {
    let rel: PathBuf = if let Some(a) = &f.agent {
        Path::new("agents").join(a).join(format!("{}.md", f.stem))
    } else {
        Path::new("shared").join(format!("{}.md", f.stem))
    };
    let path = root.join(&rel);
    // Yarışta silinmiş dosya: sessizce atla (graph yine tutarlı).
    let meta = std::fs::metadata(&path).ok()?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let (fm, body) = split_frontmatter(&text);
    let get = |k: &str| fm.get(k).cloned().unwrap_or_default();
    let name = get("name");
    Some(MemNode {
        id: f.id.clone(),
        name: if name.is_empty() { f.stem.clone() } else { name },
        scope: f.scope.clone(),
        agent_id: f.agent.clone(),
        description: get("description"),
        kind: fm.get("metadata.type").cloned().filter(|k| !k.is_empty()),
        links: links_of(&body),
        updated_at: meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0),
        size: meta.len(),
    })
}

/// `[[slug]]` çözümü: aynı scope (ajanın kendi klasörü) → shared → diğer ajanlar (ada göre).
fn resolve<'a>(slug: &str, from: &MemNode, by_id: &'a BTreeMap<String, usize>, nodes: &'a [MemNode]) -> Option<&'a MemNode> {
    if from.scope == "team" || from.id.starts_with("project/") {
        let parent = from.id.rsplit_once('/')?.0;
        let target = if slug.contains('/') { slug.into() } else { format!("{parent}/{slug}") };
        return by_id.get(&target).and_then(|&i| nodes.get(i));
    }
    if let Some(n) = by_id.get(slug).and_then(|&i| nodes.get(i)) { return Some(n); }
    if let Some(a) = &from.agent_id {
        if let Some(n) = by_id.get(&format!("agents/{a}/{slug}")).and_then(|&i| nodes.get(i)) {
            return Some(n);
        }
    } else if let Some(n) = by_id.get(&format!("shared/{slug}")).and_then(|&i| nodes.get(i)) {
        return Some(n);
    }
    if let Some(n) = by_id.get(&format!("shared/{slug}")).and_then(|&i| nodes.get(i)) {
        return Some(n);
    }
    let mut cands: Vec<&MemNode> =
        nodes.iter().filter(|n| n.scope == "agent" && n.id.ends_with(&format!("/{slug}"))).collect();
    cands.sort_by(|a, b| a.id.cmp(&b.id));
    cands.into_iter().next()
}

struct ScopedRoots {
    team_id: String,
    project_id: Option<String>,
    team: PathBuf,
    private: PathBuf,
    project: Option<PathBuf>,
}

impl ScopedRoots {
    fn scopes(&self) -> Vec<(String, &Path, bool)> {
        let mut out =
            vec![(format!("team/{}", self.team_id), self.team.as_path(), false), (format!("private/{}", self.team_id), self.private.as_path(), true)];
        if let Some((id, root)) = self.project_id.as_ref().zip(self.project.as_ref()).filter(|(_, root)| !same_dir(root, &self.team)) {
            out.push((format!("project/{id}"), root.as_path(), false));
        }
        // #2463: the selected project's worker private folders; once when it is the team's own root.
        if let Some((id, root)) = self.project_id.as_ref().zip(self.project.as_ref()).filter(|(_, root)| !same_dir(root, &self.private)) {
            out.push((format!("private/project/{id}"), root.as_path(), true));
        }
        out
    }
}

fn memory_roots(core: &crate::Core, team_id: &str, project_id: Option<&str>, default: bool) -> Result<ScopedRoots, String> {
    crate::db::with(&core.db, |c| {
        let team = crate::teams::get_team(c, &core.paths.data_dir, team_id)?;
        let project = if default { team.default_project_id.as_deref() } else { project_id };
        let ctx = crate::project_context::resolve(
            c,
            &core.paths.data_dir,
            crate::project_context::ResolveRequest {
                team_id,
                agent_id: None,
                delegation_id: None,
                project_id: project,
                mode: crate::project_context::ContextMode::Team,
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(ScopedRoots {
            team_id: team_id.into(),
            project_id: if project.is_some() { ctx.project_id } else { None },
            team: ctx.team_memory_root,
            private: ctx.agent_memory_root,
            project: if project.is_some() { ctx.project_memory_root } else { None },
        })
    })
}

pub(crate) fn visible_in_project(text: &str, project: Option<&str>) -> bool {
    let (fm, _) = split_frontmatter(text);
    fm.get("metadata.project").is_none_or(|p| Some(p.as_str()) == project)
}

fn scoped_graph(roots: &ScopedRoots) -> MemGraph {
    let mut nodes = Vec::new();
    for (prefix, root, private) in roots.scopes() {
        for mut n in graph_nodes(root) {
            if (private && n.scope != "agent") || (!private && n.scope != "shared") {
                continue;
            }
            if private
                && !path_of(root, &n.id)
                    .ok()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .is_some_and(|t| visible_in_project(&t, roots.project_id.as_deref()))
            {
                continue;
            }
            n.id = format!("{prefix}/{}", n.id);
            n.scope = prefix.split('/').next().unwrap_or("").into();
            nodes.push(n);
        }
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: BTreeSet<_> = nodes.iter().map(|n| n.id.clone()).collect();
    let mut edges = Vec::new();
    let mut missing = BTreeSet::new();
    for n in &nodes {
        let parent = n.id.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        for link in &n.links {
            // Bare slugs resolve only in the originating directory. Cross-scope links are explicit.
            let target = if link.contains('/') { link.clone() } else { format!("{parent}/{link}") };
            let to = if ids.contains(&target) {
                target
            } else {
                let id = format!("missing/{parent}/{link}");
                missing.insert(id.clone());
                id
            };
            edges.push(MemEdge { from: n.id.clone(), to });
        }
    }
    for id in missing {
        nodes.push(MemNode {
            name: id.rsplit('/').next().unwrap_or("").into(),
            id,
            scope: "missing".into(),
            agent_id: None,
            description: String::new(),
            kind: None,
            links: Vec::new(),
            updated_at: 0,
            size: 0,
        });
    }
    MemGraph { nodes, edges }
}

fn scoped_path(roots: &ScopedRoots, id: &str) -> Result<PathBuf, String> {
    for (prefix, root, private) in roots.scopes() {
        if let Some(rel) = id.strip_prefix(&format!("{prefix}/")) {
            // A team id may itself be `project`: its prefix can also cover `private/project/…`.
            if rel.starts_with("agents/") != private {
                continue;
            }
            let path = path_of(root, rel)?;
            if private && !std::fs::read_to_string(&path).is_ok_and(|t| visible_in_project(&t, roots.project_id.as_deref())) {
                break;
            }
            return Ok(path);
        }
    }
    Err("geçersiz hafıza scope/id".into())
}

fn read_doc(path: &Path, id: &str) -> Result<MemDoc, String> {
    let text = std::fs::read_to_string(path).map_err(|_| "hafıza dosyası okunamadı")?;
    let (frontmatter, body) = split_frontmatter(&text);
    Ok(MemDoc { id: id.into(), frontmatter, body })
}

fn legacy_id(shared: &str, private: &str, id: &str) -> String {
    id.strip_prefix(shared).or_else(|| id.strip_prefix(private)).unwrap_or(id).into()
}

fn map_agent_ids(core: &crate::Core, team: &str, graph: &mut MemGraph) -> Result<(), String> {
    let ids = crate::db::with(&core.db, |c| {
        let mut stmt = c.prepare("SELECT id,COALESCE(slug,id) FROM agents WHERE team_id=?1").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([team], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
        let mut ids = BTreeMap::new();
        for row in rows {
            let (id, slug) = row.map_err(|e| e.to_string())?;
            // An unmapped legacy id already is a UI id; it must not shadow another agent's held slug.
            ids.insert(slug, id);
        }
        Ok(ids)
    })?;
    for node in &mut graph.nodes {
        if let Some(id) = node.agent_id.as_ref().and_then(|slug| ids.get(slug)) {
            node.agent_id = Some(id.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
fn graph_of(root: &Path) -> MemGraph {
    legacy_edges(graph_nodes(root), None)
}

fn graph_nodes(root: &Path) -> Vec<MemNode> {
    let Ok(root_canon) = root.canonicalize() else { return Vec::new() };
    let mut files = list_dir(&root_canon, &root.join("shared"), "shared", None);
    if let Ok(rd) = std::fs::read_dir(root.join("agents")) {
        let mut agents: Vec<String> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && inside(&root_canon, p))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        agents.sort();
        for a in agents {
            files.extend(list_dir(&root_canon, &root.join("agents").join(&a), "agent", Some(&a)));
        }
    }
    let mut nodes: Vec<MemNode> = files.iter().filter_map(|f| file_meta(root, f)).collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    nodes
}

fn legacy_edges(mut nodes: Vec<MemNode>, aliases: Option<(&str, &str)>) -> MemGraph {
    let by_id: BTreeMap<String, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
    let (mut edges, mut missing) = (Vec::new(), BTreeSet::new());
    for n in &nodes {
        for slug in &n.links {
            let target = aliases.map(|(shared, private)| legacy_id(shared, private, slug)).unwrap_or_else(|| slug.clone());
            match resolve(&target, n, &by_id, &nodes) {
                Some(t) => edges.push(MemEdge { from: n.id.clone(), to: t.id.clone() }),
                None => {
                    missing.insert(format!("missing/{slug}"));
                    edges.push(MemEdge { from: n.id.clone(), to: format!("missing/{slug}") });
                }
            }
        }
    }
    for id in missing {
        nodes.push(MemNode {
            name: id.strip_prefix("missing/").unwrap_or(&id).to_string(),
            scope: "missing".into(),
            agent_id: None,
            description: String::new(),
            kind: None,
            links: Vec::new(),
            updated_at: 0,
            size: 0,
            id,
        });
    }
    MemGraph { nodes, edges }
}

/// Graph id'sini kök altındaki gerçek yola çevirir; biçim dışı her şey hata.
fn path_of(root: &Path, id: &str) -> Result<PathBuf, String> {
    // shared/<ad> ya da agents/<aid>/<ad>; önek yolda kalır (#157 F2).
    let want = if id.starts_with("shared/") { 2 } else if id.starts_with("agents/") { 3 } else { 0 };
    let parts: Vec<&str> = id.split('/').collect();
    let ok_len = want > 0 && parts.len() == want;
    let ok_parts = parts.iter().all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains(['\\', ':']));
    let stem_ok = parts.last().is_some_and(|s| !s.starts_with('.'));
    if !ok_len || !ok_parts || !stem_ok {
        return Err(format!("geçersiz hafıza id'si: {id}"));
    }
    let path = root.join(format!("{id}.md"));
    // Sembolik bağ/kaçışa karşı kanonik yol kökün içinde kalmalı.
    let canon = path.canonicalize().map_err(|_| format!("hafıza dosyası yok: {id}"))?;
    let root_canon = root.canonicalize().map_err(|e| e.to_string())?;
    if !canon.starts_with(&root_canon) || canon.extension().is_none_or(|x| !x.eq_ignore_ascii_case("md")) {
        return Err(format!("geçersiz hafıza id'si: {id}"));
    }
    Ok(canon)
}

impl crate::Core {
    pub fn memory_graph(&self, team_id: &str) -> Result<MemGraph, String> {
        let roots = memory_roots(self, team_id, None, true)?;
        let mut g = scoped_graph(&roots);
        // Legacy `shared/<stem>` always names the team share; a separate project share keeps its
        // qualified `project/<pid>/shared/<stem>` id, so one id never reads another file.
        let shared = format!("team/{team_id}/");
        let private = format!("private/{team_id}/");
        for n in &mut g.nodes {
            n.id = legacy_id(&shared, &private, &n.id);
            if n.scope == "project" || n.scope == "team" {
                n.scope = "shared".into();
            } else if n.scope == "private" {
                n.scope = "agent".into();
            }
        }
        g.nodes.retain(|n| n.scope != "missing");
        g = legacy_edges(g.nodes, Some((&shared, &private)));
        // #415: folders are named by slug; the UI keys agents by id.
        map_agent_ids(self, team_id, &mut g)?;
        Ok(g)
    }

    pub fn memory_read(&self, team_id: &str, id: &str) -> Result<MemDoc, String> {
        let roots = memory_roots(self, team_id, None, true)?;
        let qualified = if id.starts_with("shared/") {
            format!("team/{team_id}/{id}")
        } else if id.starts_with("agents/") {
            format!("private/{team_id}/{id}")
        } else {
            id.into()
        };
        read_doc(&scoped_path(&roots, &qualified)?, id)
    }

    /// Additive host APIs; G binds authenticated team identity before exposing these over RPC/MCP.
    pub fn project_memory_graph(&self, team_id: &str, project_id: &str) -> Result<MemGraph, String> {
        let mut graph = scoped_graph(&memory_roots(self, team_id, Some(project_id), false)?);
        map_agent_ids(self, team_id, &mut graph)?;
        Ok(graph)
    }

    pub fn project_memory_read(&self, team_id: &str, project_id: &str, id: &str) -> Result<MemDoc, String> {
        let roots = memory_roots(self, team_id, Some(project_id), false)?;
        read_doc(&scoped_path(&roots, id)?, id)
    }

    pub fn team_memory_graph(&self, team_id: &str) -> Result<MemGraph, String> {
        let mut graph = scoped_graph(&memory_roots(self, team_id, None, false)?);
        map_agent_ids(self, team_id, &mut graph)?;
        Ok(graph)
    }

    pub fn team_memory_read(&self, team_id: &str, id: &str) -> Result<MemDoc, String> {
        let roots = memory_roots(self, team_id, None, false)?;
        read_doc(&scoped_path(&roots, id)?, id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn search_normalizes_turkish_and_ascii_queries() {
        assert_eq!(search_tokens("İŞ IŞIK sığmadı ŞĞÜÖÇ ı i\u{307}"), ["is", "isik", "sigmadi", "sguoc", "i", "i"]);
    }

    #[test]
    fn search_snippet_keeps_match_in_long_unicode_line() {
        let query = search_tokens("sigmadi").into_iter().collect();
        let text = format!("{}-sığmadı-{}", "önce/".repeat(100), "sonra/".repeat(100));
        let snippet = search_snippet(&text, &query).unwrap();
        assert!(snippet.contains("sığmadı"), "{snippet}");
        assert!(snippet.chars().count() <= 240);
    }

    #[test]
    fn search_bm25_prefers_name_then_description_to_body() {
        let d = Tmp::new(&[
            ("shared/name.md", "---\nname: migration\ndescription: root cause\n---\nother"),
            ("shared/desc.md", "---\nname: another\ndescription: migration\n---\nother"),
            ("shared/body.md", "---\nname: another\ndescription: root cause\n---\nmigration migration migration migration"),
            ("shared/irrelevant.md", "---\nname: another\n---\nother"),
        ]);
        let scope = SearchScope {
            own: d.path().join("agents/ayla"),
            dirs: vec![SearchDir { root: d.path().into(), dir: d.path().join("shared"), project: None }],
        };
        let hits = search(&scope, "migration", 8);
        assert_eq!(hits.iter().map(|h| h.slug.as_str()).collect::<Vec<_>>(), ["name", "desc", "body"]);
        assert!(hits[2].snippet.contains("migration"));
        assert!(search(&scope, "migration", 1).len() == 1);
        assert!(search(&scope, "!!!", 8).is_empty());
    }

    #[test]
    fn search_scope_isolates_agent_team_project_and_service() {
        let d = Tmp::new(&[
            ("team/shared/team.md", "needle"),
            ("team/shared/MEMORY.md", "needle"),
            ("team/agents/ayla/own.md", "needle sığmadı"),
            ("team/agents/ayla/p1.md", "---\nmetadata:\n  project: p1\n---\nneedle"),
            ("team/agents/ayla/p2.md", "---\nmetadata:\n  project: p2\n---\nneedle"),
            ("team/agents/kaan/secret.md", "needle"),
            ("foreign/shared/foreign.md", "needle"),
            ("project/shared/project.md", "needle"),
        ]);
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let mut ctx = crate::project_context::resolve(&c, d.path(), crate::project_context::ResolveRequest {
            team_id: "default", agent_id: Some("ayla"), delegation_id: None, project_id: None,
            mode: crate::project_context::ContextMode::Team,
        }).unwrap();
        ctx.team_memory_root = d.path().join("team");
        ctx.agent_memory_root = ctx.team_memory_root.clone();
        ctx.project_memory_root = Some(d.path().join("project"));
        ctx.project_id = Some("p1".into());
        let scope = SearchScope::for_context(&c, &ctx).unwrap();
        let mut slugs: Vec<_> = search(&scope, "needle", 8).into_iter().map(|h| h.slug).collect();
        slugs.sort();
        assert_eq!(slugs, ["own", "p1", "project", "team"]);
        assert_eq!(search(&scope, "sigmadi", 8)[0].slug, "own");
        ctx.project_team_id = Some("foreign".into());
        assert!(!search(&SearchScope::for_context(&c, &ctx).unwrap(), "needle", 8).iter().any(|h| h.slug == "project"));
        ctx.project_id = None;
        assert!(!search(&SearchScope::for_context(&c, &ctx).unwrap(), "needle", 8).iter().any(|h| h.slug == "p1"));
        std::fs::write(d.path().join("team/shared/fresh.md"), "needle").unwrap();
        assert!(search(&scope, "needle", 8).iter().any(|h| h.slug == "fresh"), "files remain the source of truth");
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(files: &[(&str, &str)]) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!("dex-mem-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            for (rel, text) in files {
                let p = dir.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::File::create(&p).unwrap().write_all(text.as_bytes()).unwrap();
            }
            Tmp(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn frontmatter_split_and_links() {
        let (fm, body) = split_frontmatter("---\nname: a\nmetadata:\n  type: project\n---\nMetin [[x]] ve [[y|etiket]] ve [[x]].\n");
        assert_eq!(fm.get("name").map(String::as_str), Some("a"));
        assert_eq!(fm.get("metadata.type").map(String::as_str), Some("project"));
        assert_eq!(body, "Metin [[x]] ve [[y|etiket]] ve [[x]].");
        assert_eq!(links_of(&body), vec!["x".to_string(), "y".to_string()]);
        let (fm2, body2) = split_frontmatter("düz metin");
        assert!(fm2.is_empty());
        assert_eq!(body2, "düz metin");
    }

    #[test]
    fn qualified_scopes_do_not_merge_slugs_or_escape_roots() {
        let team = Tmp::new(&[("shared/same.md", "---\nname: team\n---\n[[same]] [[only-project]]")]);
        let project = Tmp::new(&[("shared/same.md", "---\nname: project\n---\n[[same]]"), ("shared/only-project.md", "x")]);
        let private = Tmp::new(&[
            ("agents/a/same.md", "---\nname: private\n---\n[[same]]"),
            ("agents/a/hidden.md", "---\nmetadata:\n  project: other\n---\nx"),
        ]);
        let roots = ScopedRoots {
            team_id: "t".into(),
            project_id: Some("p".into()),
            team: team.path().into(),
            private: private.path().into(),
            project: Some(project.path().into()),
        };
        let g = scoped_graph(&roots);
        for id in ["team/t/shared/same", "project/p/shared/same", "private/t/agents/a/same"] {
            assert!(g.nodes.iter().any(|n| n.id == id));
            assert!(g.edges.iter().any(|e| e.from == id && e.to == id));
            assert!(scoped_path(&roots, id).is_ok());
        }
        assert!(!g.edges.iter().any(|e| e.from == "team/t/shared/same" && e.to == "project/p/shared/only-project"));
        assert!(!g.nodes.iter().any(|n| n.id.ends_with("/hidden")));
        for id in
            ["project/other/shared/same", "team/t/shared/../same", "private/t/agents/a/hidden", "team/t/shared/same:stream", "team/t/shared/.hidden"]
        {
            assert!(scoped_path(&roots, id).is_err(), "{id}");
        }
        let same = ScopedRoots { project: Some(team.path().into()), ..roots };
        assert!(!scoped_graph(&same).nodes.iter().any(|n| n.scope == "project"));
    }

    #[test]
    fn legacy_and_scoped_core_apis_keep_default_view() {
        // Core listeners retain DB handles; like the existing hook integration fixtures,
        // keep this isolated workspace scratch directory until the test process exits.
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!(
            "memory-core-{}-{}",
            std::process::id(),
            crate::agents::next_seq()
        ));
        let repo = scratch.join("legacy");
        let other = scratch.join("other");
        for root in [&repo, &other] {
            std::fs::create_dir_all(root).unwrap();
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["init", "--quiet", "--initial-branch=fixture-main"])
                .status()
                .unwrap()
                .success());
        }
        let memory = repo.join(".dex/memory");
        std::fs::create_dir_all(memory.join("shared")).unwrap();
        std::fs::create_dir_all(memory.join("agents/ayla")).unwrap();
        std::fs::write(memory.join("shared/same.md"), "---\nname: shared\n---\n[[team/default/shared/same]]").unwrap();
        std::fs::write(memory.join("agents/ayla/note.md"), "---\nname: private\n---\n[[same]]").unwrap();
        let core = crate::Core::start(
            crate::Paths { data_dir: scratch.join("data"), config_dir: scratch.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let (p, q) = crate::db::with(&core.db, |c| {
            let create = |name: &str, root: &Path| {
                let ready = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
                crate::projects::create(c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &ready)
                    .map_err(|e| e.to_string())
            };
            let p = create("Legacy", &repo)?;
            let q = create("Other", &other)?;
            crate::projects::set_default(c, "default", Some(&p.id)).map_err(|e| e.to_string())?;
            c.execute("UPDATE teams SET team_memory_root=?1,agent_memory_root=?1 WHERE id='default'", [memory.to_string_lossy()]).unwrap();
            Ok((p, q))
        })
        .unwrap();
        let legacy = core.memory_graph("default").unwrap();
        assert!(legacy.nodes.iter().any(|n| n.id == "shared/same"));
        assert!(legacy.edges.iter().any(|e| e.from == "shared/same" && e.to == "shared/same"), "legacy API resolves qualified links in the default view");
        assert!(legacy.edges.iter().any(|e| e.from == "agents/ayla/note" && e.to == "shared/same"), "legacy bare links preserve fallback");
        assert_eq!(core.memory_read("default", "agents/ayla/note").unwrap().body, "[[same]]");
        let scoped = core.project_memory_graph("default", &p.id).unwrap();
        assert!(scoped.nodes.iter().any(|n| n.id == "team/default/shared/same"));
        assert!(!scoped.nodes.iter().any(|n| n.scope == "project"));
        assert!(core.project_memory_read("default", &p.id, "team/default/shared/same").is_ok());
        assert!(core.project_memory_read("default", "foreign", "team/default/shared/same").is_err());
        assert!(core.project_memory_read("default", &q.id, &format!("project/{}/shared/same", p.id)).is_err());
        assert!(core.team_memory_read("default", "team/default/shared/same").is_ok());
        assert!(core.team_memory_graph("default").unwrap().nodes.iter().all(|n| n.scope != "project"));
        crate::db::with(&core.db, |c| {
            c.execute("UPDATE agents SET slug='backend-x' WHERE id='ayla'", []).unwrap();
            c.execute("UPDATE agents SET slug='ayla' WHERE id='kaan'", []).unwrap();
            Ok(())
        })
        .unwrap();
        let claimed = core.team_memory_graph("default").unwrap();
        assert_eq!(
            claimed.nodes.iter().find(|n| n.id == "private/default/agents/ayla/note").unwrap().agent_id.as_deref(),
            Some("kaan"),
            "a held slug owns the directory, even if it equals another agent's old id"
        );
        core.shutdown();
    }

    #[test]
    fn legacy_shared_id_stays_team_when_project_share_is_separate() {
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp").join(format!(
            "memory-split-{}-{}",
            std::process::id(),
            crate::agents::next_seq()
        ));
        let repo = scratch.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "--quiet", "--initial-branch=fixture-main"])
            .status()
            .unwrap()
            .success());
        let team = scratch.join("team-memory");
        let project = repo.join(".dex/memory");
        for (root, body) in [(&team, "team body [[project-only]]"), (&project, "project body [[project-only]]")] {
            std::fs::create_dir_all(root.join("shared")).unwrap();
            std::fs::write(root.join("shared/same.md"), format!("---\nname: same\n---\n{body}")).unwrap();
        }
        std::fs::write(project.join("shared/project-only.md"), "---\nname: project-only\n---\nx").unwrap();
        let core = crate::Core::start(
            crate::Paths { data_dir: scratch.join("data"), config_dir: scratch.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let p = crate::db::with(&core.db, |c| {
            let ready = crate::projects::prepare_root(repo.to_str().unwrap(), None).unwrap();
            let p = crate::projects::create(c, "default", crate::projects::NewProject { name: "Repo".into(), worktrees: None }, &ready)
                .map_err(|e| e.to_string())?;
            crate::projects::set_default(c, "default", Some(&p.id)).map_err(|e| e.to_string())?;
            c.execute("UPDATE teams SET team_memory_root=?1,agent_memory_root=?1 WHERE id='default'", [team.to_string_lossy()]).unwrap();
            Ok(p)
        })
        .unwrap();
        let qualified = format!("project/{}/shared/same", p.id);
        assert_eq!(core.memory_read("default", "shared/same").unwrap().body, "team body [[project-only]]", "old id = team shared");
        let doc = core.memory_read("default", &qualified).unwrap();
        assert_eq!(doc.body, "project body [[project-only]]");
        assert_eq!(doc.id, qualified);
        let g = core.memory_graph("default").unwrap();
        let node = |id: &str| g.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id}: {:?}", g.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()));
        assert_eq!(node("shared/same").scope, "shared");
        assert_eq!(node(&qualified).scope, "shared", "old UI scope set");
        assert!(g.nodes.iter().all(|n| ["shared", "agent", "missing"].contains(&n.scope.as_str())));
        assert!(!g.nodes.iter().any(|n| n.id.starts_with("team/")));
        let project_only = format!("project/{}/shared/project-only", p.id);
        assert!(g.edges.iter().any(|e| e.from == qualified && e.to == project_only), "project bare link stays in its folder");
        assert!(g.edges.iter().any(|e| e.from == "shared/same" && e.to == "missing/project-only"), "team bare link does not reach the project");
        core.shutdown();
    }

    #[test]
    fn resolution_order_and_missing() {
        let d = Tmp::new(&[
            ("shared/a.md", "---\nname: a\n---\n[[b]] [[ghost]]\n"),
            ("shared/b.md", "---\nname: b\n---\nx\n"),
            ("agents/timur/b.md", "---\nname: b-timur\n---\ny\n"),
            ("agents/timur/c.md", "---\nname: c\n---\n[[b]] [[a]]\n"),
        ]);
        let g = graph_of(d.path());
        let edge = |f: &str, t: &str| g.edges.iter().any(|e| e.from == f && e.to == t);
        // shared/a → shared/b (kendi scope'u shared)
        assert!(edge("shared/a", "shared/b"));
        // timur/c → kendi klasöründeki b önce
        assert!(edge("agents/timur/c", "agents/timur/b"));
        assert!(edge("agents/timur/c", "shared/a"));
        // çözülemeyen → missing hayalet node, edges'te
        assert!(edge("shared/a", "missing/ghost"));
        let m = g.nodes.iter().find(|n| n.id == "missing/ghost").unwrap();
        assert_eq!(m.scope, "missing");
        assert!(m.agent_id.is_none() && m.description.is_empty());
        assert!(g.nodes.iter().all(|n| n.scope == "shared" || n.scope == "agent" || n.scope == "missing"));
    }

    #[test]
    fn path_traversal_rejected() {
        let d = Tmp::new(&[("shared/a.md", "---\nname: a\n---\nx\n")]);
        for bad in [
            "../x",
            "shared/../a",
            "shared/",
            "agents/t/a/b",
            "/etc/passwd",
            "missing/ghost",
            "shared/.gizli",
            r"shared\a",
            "",
        ] {
            assert!(path_of(d.path(), bad).is_err(), "{bad}");
        }
        assert!(path_of(d.path(), "shared/a").is_ok());
    }

    #[test]
    fn agent_and_shared_ids_read() {
        let d = Tmp::new(&[
            ("shared/a.md", "---
name: a
---
ortak
"),
            ("agents/timur/b.md", "---
name: b
---
özel
"),
            // Önek sıyrılırsa bulunacak yanlış yer (#157 F2): okunmamalı.
            ("timur/b.md", "---
name: b
---
yanlış
"),
        ]);
        let read = |id: &str| std::fs::read_to_string(path_of(d.path(), id).unwrap()).unwrap();
        assert!(read("agents/timur/b").contains("özel"));
        assert!(read("shared/a").contains("ortak"));
        for bad in ["agents/timur", "agents/timur/../b", "agents/../shared/a", "timur/b", "shared/x/a"] {
            assert!(path_of(d.path(), bad).is_err(), "{bad}");
        }
        // Graph'ın döndürdüğü her id okunabilir.
        for n in graph_of(d.path()).nodes {
            assert!(path_of(d.path(), &n.id).is_ok(), "{}", n.id);
        }
    }

    /// Dizin bağı: Windows'ta junction (yetki istemez), Unix'te symlink.
    pub(crate) fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            // mklink "/" ayırıcısını kabul etmez.
            let win = |p: &Path| p.to_string_lossy().replace('/', "\\");
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(win(link))
                .arg(win(target))
                .output()
                .is_ok_and(|o| o.status.success())
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    fn link_file(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    #[test]
    fn links_escaping_root_skipped() {
        let out = Tmp::new(&[("secret.md", "---
name: secret
---
gizli
")]);
        let d = Tmp::new(&[("shared/a.md", "---
name: a
---
x
"), ("agents/iç/b.md", "---
name: b
---
y
")]);
        assert!(link_dir(out.path(), &d.path().join("agents/evil")), "junction/symlink kurulamadı");
        // Kök içinde kalan bağ izlenir.
        assert!(link_dir(&d.path().join("agents/iç"), &d.path().join("agents/ayna")));
        // Dosya symlink'i Windows'ta yetki ister; kurulamazsa o kısım atlanır.
        let file_link = link_file(&out.path().join("secret.md"), &d.path().join("shared/kacak.md"));
        let g = graph_of(d.path());
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.iter().any(|i| i.starts_with("agents/evil")), "{ids:?}");
        assert!(ids.contains(&"agents/ayna/b") && ids.contains(&"agents/iç/b") && ids.contains(&"shared/a"), "{ids:?}");
        assert!(path_of(d.path(), "agents/evil/secret").is_err());
        if file_link {
            assert!(!ids.contains(&"shared/kacak"), "{ids:?}");
            assert!(path_of(d.path(), "shared/kacak").is_err());
        }
        // Junction'ı hedefiyle birlikte silmemek için önce bağları kaldır.
        let _ = std::fs::remove_dir(d.path().join("agents/evil"));
        let _ = std::fs::remove_dir(d.path().join("agents/ayna"));
    }
}
