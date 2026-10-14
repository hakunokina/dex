//! Role catalog (DEX-12). Source of truth is `seed/roles.json`, a snapshot of the Stable
//! Supabase `app.role_templates` table; the app never talks to Supabase at runtime.
//! `seed` upserts it after every migrate, so a new build refreshes names/prompts in place.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
const SEED: &str = include_str!("../seed/roles.json");

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Role {
    pub slug: String,
    pub name: String,
    pub category: String,
    pub description: String,
    #[serde(alias = "system_prompt", skip_serializing)]
    pub system_prompt: String,
}

pub fn seed(conn: &Connection) -> rusqlite::Result<()> {
    let roles: Vec<Role> =
        serde_json::from_str(SEED).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let mut st = conn.prepare(
        "INSERT INTO roles (slug, name, category, description, system_prompt, sort)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(slug) DO UPDATE SET name = excluded.name, category = excluded.category,
             description = excluded.description,
             system_prompt = excluded.system_prompt, sort = excluded.sort",
    )?;
    for (i, r) in roles.iter().enumerate() {
        st.execute(params![r.slug, r.name, r.category, r.description, r.system_prompt, i as i64])?;
    }
    Ok(())
}

/// DEX-15: migration 2 seeded the first crew with free-text titles; a title is the role's name now.
/// Runs on every open but only rewrites rows still carrying that exact seed title, so it is a no-op
/// after the first run and never touches a title the user set. Not a migration: it needs the role
/// names, which `seed` writes after migrating (and fresh databases get the old titles from migration 2).
pub fn retitle_seed_crew(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE agents SET title = (SELECT name FROM roles WHERE slug = agents.role_slug)
         WHERE role_slug IN (SELECT slug FROM roles)
           AND ((id = 'timur' AND title = 'Takım Lideri')
             OR (id IN ('kaan', 'ayla', 'bora') AND title = 'Geliştirici'))",
        [],
    )
}

/// Catalog order = seed order (grouped by category). Prompts stay in Rust.
pub fn list(conn: &Connection) -> Result<Vec<Role>, String> {
    let mut st = conn
        .prepare("SELECT slug, name, category, description FROM roles ORDER BY sort, slug")
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok(Role {
                slug: r.get(0)?,
                name: r.get(1)?,
                category: r.get(2)?,
                description: r.get(3)?,
                system_prompt: String::new(),
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// `slug — ad, slug — ad, …` in catalog order, built from the table (never a fixed list): the leader's
/// rules and the unknown-role errors show the same text.
pub fn catalog_text(conn: &Connection) -> String {
    catalog_text_for_profile(conn, "dev")
}

/// Education and development leaders see only their own profile's roles.
pub fn catalog_text_for_profile(conn: &Connection, profile: &str) -> String {
    conn.prepare("SELECT slug || ' — ' || name FROM roles WHERE slug <> 'assistant' AND (category='education') = ?1 ORDER BY sort, slug")
        .and_then(|mut st| st.query_map([profile == "edu"], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>())
        .unwrap_or_default()
        .join(", ")
}

/// Is `key` a catalog slug or role name (case-insensitive)?
pub fn in_catalog(conn: &Connection, key: &str) -> bool {
    let k = key.trim().to_lowercase();
    conn.query_row("SELECT 1 FROM roles WHERE lower(slug) = ?1 OR lower(name) = ?1", [&k], |_| Ok(()))
        .optional()
        .ok()
        .flatten()
        .is_some()
}

pub fn name_of(conn: &Connection, slug: &str) -> Result<String, String> {
    conn.query_row("SELECT name FROM roles WHERE slug = ?1", [slug], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("rol yok: {slug}"))
}

/// The system prompt of the agent's role; None for unknown agents / no role.
pub fn prompt_for_agent(conn: &Connection, agent_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT r.system_prompt FROM agents a JOIN roles r ON r.slug = a.role_slug WHERE a.id = ?1",
        [agent_id],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// DEX-20: whether the agent's role carries `"browser": true` in the catalog (→ browser MCP at
/// spawn). Read from the embedded seed, not a column: only the spawn needs it.
pub fn browser_for_agent(conn: &Connection, agent_id: &str) -> bool {
    #[derive(Deserialize)]
    struct Flag {
        slug: String,
        #[serde(default)]
        browser: bool,
    }
    static SLUGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let slugs = SLUGS.get_or_init(|| {
        let all: Vec<Flag> = serde_json::from_str(SEED).unwrap_or_default();
        all.into_iter().filter(|f| f.browser).map(|f| f.slug).collect()
    });
    conn.query_row("SELECT role_slug FROM agents WHERE id = ?1", [agent_id], |r| r.get::<_, Option<String>>(0))
        .optional()
        .ok()
        .flatten()
        .flatten()
        .is_some_and(|s| slugs.contains(&s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn assistant_seed_upserts_without_adding_members_and_is_not_delegatable() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let prompt = || c.query_row("SELECT system_prompt FROM roles WHERE slug='assistant'", [], |r| r.get::<_, String>(0)).unwrap();
        let task_line = "\nMesaj `Görev:` satırıyla başlarsa: Repo incele → repoyu bul ve açıkla; Kaynak doğrula → iddiaları kaynakla kontrol et; Soru → serbest cevap.";
        let format_line = "\nCevabı markdown yaz: ilk satır tek cümlelik sonuç; sonra kısa bölümler (repo için: Ne yapar / Bağlantı / Lisans / Bakım / Öne çıkanlar; doğrulama için: İddia → Kaynak → Sonuç); her iddiaya link; HTML kullanma.";
        let current = prompt();
        assert!(current.contains(task_line) && current.ends_with(format_line));
        // #1717: an existing DB with the pre-task prompt gets the share-task and markdown lines on the next seed.
        c.execute("UPDATE roles SET system_prompt=?1 WHERE slug='assistant'", [current.replace(task_line, "").replace(format_line, "")]).unwrap();
        seed(&c).unwrap();
        assert_eq!(prompt(), current);
        c.execute("DELETE FROM roles WHERE slug='assistant'",[]).unwrap(); // existing 22-role DB
        assert_eq!(list(&c).unwrap().len(),22);
        let before: i64 = c.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0)).unwrap();
        for _ in 0..2 {
            seed(&c).unwrap();
            assert_eq!(name_of(&c, "assistant").unwrap(), "Assistant");
            assert_eq!(prompt(), current);
            assert_eq!(list(&c).unwrap().len(), 23);
            assert_eq!(c.query_row("SELECT COUNT(*) FROM agents", [], |r| r.get::<_, i64>(0)).unwrap(), before);
            for profile in ["dev", "edu"] { assert!(!catalog_text_for_profile(&c, profile).contains("assistant")); }
        }
        c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).unwrap();
        assert!(crate::delegate::role_target(&c, "default", "assistant").is_err());
        assert!(crate::delegate::role_target(&c, "default", "Ayla").is_err());
        assert_ne!(crate::delegate::pick_worker(&c, "default", None).unwrap(), "ayla");
    }

    #[test]
    fn seed_loads_catalog_and_is_idempotent() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        seed(&c).unwrap();
        let roles = list(&c).unwrap();
        assert_eq!(roles.len(), 23);
        for s in ["lead", "frontend", "backend", "qa", "explorer", "game-designer", "technical-artist", "gameplay-programmer"] {
            assert!(roles.iter().any(|r| r.slug == s), "{s}");
        }
        assert_eq!(name_of(&c, "lead").unwrap(), "Lead");
        assert!(prompt_for_agent(&c, "timur").unwrap().starts_with("Rolün: Takım Lideri"));
        assert!(prompt_for_agent(&c, "yok").is_none());
    }

    #[test]
    fn seed_refreshes_existing_visual_explainer_prompt_twice() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let prompt = |slug: &str| c.query_row("SELECT system_prompt FROM roles WHERE slug=?1", [slug], |r| r.get::<_, String>(0)).unwrap();
        let count = || c.query_row("SELECT COUNT(*) FROM roles", [], |r| r.get::<_, i64>(0)).unwrap();
        let instruction = "Konu mimari, iş akışı, sıralama, veri akışı ya da durum makinesi ise ve `archify` skill'i kullanılabiliyorsa onu kullan; kullanılamıyorsa ya da konu bu türden değilse serbest HTML üret.";
        let current = prompt("visual-explainer");
        assert!(current.contains(instruction));
        let legacy = current.replace(&format!(" {instruction}"), "");
        assert_ne!(legacy, current);
        c.execute("UPDATE roles SET system_prompt=?1 WHERE slug='visual-explainer'", [&legacy]).unwrap();
        let before = count();
        let teacher = prompt("teacher");
        for _ in 0..2 {
            seed(&c).unwrap();
            assert_eq!(prompt("visual-explainer"), current);
            assert_eq!(prompt("teacher"), teacher);
            assert_eq!(count(), before);
        }
        assert!(current.contains("Takımın ortak şablonunu (`lessons/_template.html`) kullan; yoksa ilk işte oluştur, sonrakilerde ona uy."));
        assert!(current.contains("`lessons/<sıra>-<konu>.html`"));
    }

    #[test]
    fn catalogs_filter_education_in_both_directions_and_keep_order() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        let roles = list(&c).unwrap();
        for profile in ["dev", "edu"] {
            let expected = roles.iter().filter(|r| r.slug != "assistant" && (r.category == "education") == (profile == "edu"))
                .map(|r| format!("{} — {}", r.slug, r.name)).collect::<Vec<_>>().join(", ");
            assert_eq!(catalog_text_for_profile(&c, profile), expected);
        }
        assert_eq!(catalog_text_for_profile(&c, "edu"), "teacher — Teacher, visual-explainer — Visual Explainer");
        assert_eq!(catalog_text(&c), catalog_text_for_profile(&c, "dev"));
    }

    #[test]
    fn seed_crew_titles_become_role_names_once() {
        let c = crate::db::open(Path::new(":memory:")).unwrap(); // open() already retitled
        let title = |id: &str| c.query_row("SELECT title FROM agents WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(title("timur"), "Lead");
        assert_eq!(title("kaan"), name_of(&c, "frontend").unwrap());
        assert_eq!(title("ayla"), name_of(&c, "backend").unwrap());
        assert_eq!(title("bora"), name_of(&c, "qa").unwrap());
        assert_eq!(retitle_seed_crew(&c).unwrap(), 0); // idempotent
        // A title the user chose afterwards survives the next start.
        c.execute("UPDATE agents SET title = 'Özel' WHERE id = 'kaan'", []).unwrap();
        assert_eq!(retitle_seed_crew(&c).unwrap(), 0);
        assert_eq!(title("kaan"), "Özel");
    }

    #[test]
    fn browser_only_for_flagged_roles() {
        let c = crate::db::open(Path::new(":memory:")).unwrap();
        assert!(browser_for_agent(&c, "kaan")); // frontend
        assert!(browser_for_agent(&c, "bora")); // qa
        assert!(!browser_for_agent(&c, "ayla")); // backend
        assert!(!browser_for_agent(&c, "timur")); // lead
        assert!(!browser_for_agent(&c, "yok"));
    }
}
