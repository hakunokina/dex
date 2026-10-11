//! Process skills (DEX-23): four workflow skills embedded in the binary and loaded
//! per-role into Claude sessions.
//!
//! Only the four skills Timur marked "net gerekecek" are here; the roles'
//! wider `preset.skills` lists are deliberately NOT imported. A fifth skill later
//! is: one `SKILL.md` dir + one `role_skills` row per role ("cımbızla ekleme").
//!
//! Loading: at spawn, [`Skills::plugin_dir_for_agent`] materializes a per-process
//! plugin dir (`<config_dir>/dex-skills-<pid>/dex-<role>/`) holding only that
//! agent's role skills, and `pty::spawn` passes it as `claude --plugin-dir`.
//! Nothing is written to the user's `~/.claude/skills` or the project `.claude/`.
//! Skill contents live in `crates/dex-core/skills/<name>/SKILL.md` and are baked in
//! with `include_str!`, so the S4 server build carries them too.

use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

/// Skill name → embedded SKILL.md body. The dir name IS the skill name; the
/// frontmatter `name` must match it (asserted in tests).
pub const ALL: &[(&str, &str)] = &[
    ("systematic-debugging", include_str!("../skills/systematic-debugging/SKILL.md")),
    ("test-driven-development", include_str!("../skills/test-driven-development/SKILL.md")),
    ("requesting-code-review", include_str!("../skills/requesting-code-review/SKILL.md")),
    ("one-three-one-rule", include_str!("../skills/one-three-one-rule/SKILL.md")),
];

fn body(name: &str) -> Option<&'static str> {
    ALL.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// Role → skill mapping as data (mirrors the `role_skills` table seeded by
/// migration 8; the table is the source of truth at runtime).
pub fn skills_for_role(conn: &Connection, role: &str) -> Result<Vec<String>, String> {
    let mut st = conn
        .prepare("SELECT skill FROM role_skills WHERE role_slug = ?1 ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let rows = st.query_map([role], |r| r.get(0)).map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// Skills for the agent's own role; empty (not an error) when the agent or its
/// role carries none — e.g. marketing, or a role-less agent.
pub fn skills_for_agent(conn: &Connection, agent: &str) -> Result<Vec<String>, String> {
    let role: Option<String> = conn
        .query_row("SELECT role_slug FROM agents WHERE id = ?1", [agent], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    match role {
        Some(r) => skills_for_role(conn, &r),
        None => Ok(Vec::new()),
    }
}

/// Per-process plugin root (`<config_dir>/dex-skills-<pid>`), created at startup
/// and removed on shutdown — same lifecycle as the hook settings and MCP config.
pub struct Skills {
    pub root: PathBuf,
}

/// Binds the root dir and writes nothing yet (per-role plugin dirs are
/// materialized lazily at spawn, so a runtime `role_skills` edit takes effect
/// on the next spawn). Sweeps roots of dead processes first.
pub fn start(config_dir: &Path) -> Result<Skills, String> {
    let e = |e: std::io::Error| e.to_string();
    std::fs::create_dir_all(config_dir).map_err(e)?;
    sweep_stale(config_dir);
    let root = config_dir.join(format!("dex-skills-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(e)?;
    Ok(Skills { root })
}

impl Skills {
    /// Same per-process lifecycle as the role skill plugins; shared by all leaders.
    pub fn leader_colors_dir(&self) -> Result<PathBuf, String> {
        let dir = self.root.join("dex-leader-colors");
        for (path, text) in [
            (".claude-plugin/plugin.json", include_str!("../mods/dex-leader-colors/.claude-plugin/plugin.json")),
            ("hooks/hooks.json", include_str!("../mods/dex-leader-colors/hooks/hooks.json")),
            ("hooks/register.tsx", include_str!("../mods/dex-leader-colors/hooks/register.tsx")),
            ("hooks/sections.ts", include_str!("../mods/dex-leader-colors/hooks/sections.ts")),
        ] {
            write_if_different(&dir.join(path), text)?;
        }
        Ok(dir)
    }

    pub fn remove_all(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }

    /// Materializes (creating or refreshing) the plugin dir for `role` and
    /// returns it; None when the role has no skills. Unknown skill names in the
    /// table are skipped, never fatal — a later migration adds the content.
    pub fn plugin_dir(&self, conn: &Connection, role: &str) -> Result<Option<PathBuf>, String> {
        let skills = skills_for_role(conn, role)?;
        let known: Vec<(&str, &str)> = skills.iter().filter_map(|s| body(s).map(|b| (s.as_str(), b))).collect();
        if known.is_empty() {
            return Ok(None);
        }
        let dir = self.root.join(format!("dex-{role}"));
        write_if_different(
            &dir.join(".claude-plugin").join("plugin.json"),
            &serde_json::json!({
                "name": format!("dex-{role}"),
                "description": format!("Dex {role} rolü süreç skill'leri"),
                "version": "1.0.0"
            })
            .to_string(),
        )?;
        for (name, text) in known {
            write_if_different(&dir.join("skills").join(name).join("SKILL.md"), text)?;
        }
        Ok(Some(dir))
    }

    /// [`Skills::plugin_dir`] for the agent's own role.
    pub fn plugin_dir_for_agent(&self, conn: &Connection, agent: &str) -> Result<Option<PathBuf>, String> {
        let role: Option<String> = conn
            .query_row("SELECT role_slug FROM agents WHERE id = ?1", [agent], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        match role {
            Some(r) => self.plugin_dir(conn, &r),
            None => Ok(None),
        }
    }
}

fn write_if_different(path: &Path, text: &str) -> Result<(), String> {
    let e = |e: std::io::Error| e.to_string();
    if path.is_file() && std::fs::read_to_string(path).map_err(e).map(|old| old == text).unwrap_or(false) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(e)?;
    }
    std::fs::write(path, text).map_err(e)
}

fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid = name.to_string_lossy().strip_prefix("dex-skills-").and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|p| !crate::proc::alive(p)) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The mapping Timur approved for DEX-23: skill → roles.
    const EXPECTED: &[(&str, &[&str])] = &[
        ("systematic-debugging", &["backend", "frontend", "data-engineer", "devops", "qa", "code-automation"]),
        ("test-driven-development", &["backend", "frontend", "data-engineer", "code-automation"]),
        ("requesting-code-review", &["code-review", "backend", "frontend"]),
        ("one-three-one-rule", &["lead", "pm"]),
    ];

    #[test]
    fn leader_color_mod_is_embedded_and_refreshed() {
        let root = std::env::current_dir().unwrap().join("../../.dex/tmp/leader-colors-install-test");
        let s = Skills { root };
        let dir = s.leader_colors_dir().unwrap();
        let manifest = dir.join(".claude-plugin/plugin.json");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&manifest).unwrap()).unwrap()["name"], "dex-leader-colors");
        for file in ["hooks/hooks.json", "hooks/register.tsx", "hooks/sections.ts"] {
            assert!(!std::fs::read_to_string(dir.join(file)).unwrap().is_empty());
        }
        std::fs::write(dir.join("hooks/sections.ts"), "stale").unwrap();
        assert_eq!(s.leader_colors_dir().unwrap(), dir);
        assert!(std::fs::read_to_string(dir.join("hooks/sections.ts")).unwrap().contains("splitSections"));
        s.remove_all();
    }

    #[test]
    fn role_skill_table_matches_approved_mapping() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        for (skill, roles) in EXPECTED {
            for role in *roles {
                let got = skills_for_role(&c, role).unwrap();
                assert!(got.contains(&skill.to_string()), "{role} should carry {skill}: {got:?}");
            }
        }
        // Marketing gets none; unknown roles / agents are empty, not errors.
        assert!(skills_for_role(&c, "marketing").unwrap().is_empty());
        assert!(skills_for_role(&c, "yok").unwrap().is_empty());
        assert!(skills_for_agent(&c, "yok").unwrap().is_empty());
        // Only the four skills exist anywhere in the table.
        let n: i64 = c.query_row("SELECT COUNT(DISTINCT skill) FROM role_skills", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn backend_gets_three_lead_gets_one() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        assert_eq!(
            skills_for_role(&c, "backend").unwrap(),
            vec!["systematic-debugging", "test-driven-development", "requesting-code-review"]
        );
        assert_eq!(skills_for_role(&c, "lead").unwrap(), vec!["one-three-one-rule"]);
    }

    #[test]
    fn plugin_dirs_are_role_scoped() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let root = std::env::temp_dir().join(format!("dex-skills-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let s = Skills { root: root.clone() };

        let back = s.plugin_dir(&c, "backend").unwrap().expect("backend has skills");
        assert!(back.join("skills").join("systematic-debugging").join("SKILL.md").is_file());
        assert!(back.join("skills").join("test-driven-development").join("SKILL.md").is_file());
        assert!(back.join("skills").join("requesting-code-review").join("SKILL.md").is_file());
        assert!(!back.join("skills").join("one-three-one-rule").exists());
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(back.join(".claude-plugin").join("plugin.json")).unwrap()).unwrap();
        assert_eq!(manifest["name"], "dex-backend");

        let lead = s.plugin_dir(&c, "lead").unwrap().expect("lead has skills");
        assert!(lead.join("skills").join("one-three-one-rule").join("SKILL.md").is_file());
        assert!(!lead.join("skills").join("systematic-debugging").exists());

        assert!(s.plugin_dir(&c, "marketing").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn embedded_contents_are_dex_shaped() {
        for (name, text) in ALL {
            // Folder name and frontmatter name agree (Claude resolves by folder).
            assert!(text.starts_with(&format!("---\nname: {name}\n")), "{name} frontmatter");
            assert!(text.contains("description:"), "{name} needs a description for auto-invocation");
        }
        let all = ALL.iter().map(|(_, b)| *b).collect::<Vec<_>>().join("\n");
        // No foreign workflow concepts leaked in: Dex MCP is task_*/delegate/report/plan_*.
        for leak in ["agentspace_", "AgentSpace", "create_task", "codebase-memory", "/code-review"] {
            assert!(!all.contains(leak), "leak: {leak}");
        }
    }
}
