//! Where the core keeps its files. The desktop host uses `desktop_storage`; `dexd` receives
//! those selected paths through flags. Standalone servers keep their own data directory.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Paths {
    /// `dex.db`, service-team workspaces and memory.
    pub data_dir: PathBuf,
    /// `layout.json`, per-process hook/MCP settings files.
    pub config_dir: PathBuf,
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    use crate::desktop_storage::{select_at, IDENTIFIER, LEGACY_IDENTIFIER};
    use std::path::Path;

    fn fixture(name: &str) -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dex/tmp")
            .join(format!(
                "storage-core-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn legacy_database_and_layout_are_read_and_written_in_place_on_repeated_open() {
        let root = fixture("db-layout");
        let data_base = root.join("data");
        let config_base = root.join("config");
        let local_base = root.join("local");
        let old = data_base.join(LEGACY_IDENTIFIER);
        let old_config = config_base.join(LEGACY_IDENTIFIER);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&old_config).unwrap();
        let conn = crate::db::open(&old.join("dex.db")).unwrap();
        conn.execute_batch("CREATE TABLE storage_fixture(id INTEGER PRIMARY KEY); INSERT INTO storage_fixture VALUES(1),(2);").unwrap();
        let workspace = old.join("teams/default/workspace");
        conn.execute(
            "UPDATE teams SET cwd=?1,workspace_root=?1",
            [workspace.to_str()],
        )
        .unwrap();
        let team_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM teams", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        let layout = serde_json::json!({"teams":{"default":{"shells":[{"cwd":workspace}]}}});
        let original_layout = serde_json::to_vec(&layout).unwrap();
        std::fs::write(old_config.join("layout.json"), &original_layout).unwrap();
        for id in [3, 4] {
            let selected = select_at(IDENTIFIER, &data_base, &config_base, &local_base);
            let paths = Paths {
                data_dir: selected.data_dir,
                config_dir: selected.config_dir,
            };
            assert_eq!(paths.data_dir, old);
            assert_eq!(paths.config_dir, old_config);
            let db = crate::db::open(&paths.data_dir.join("dex.db")).unwrap();
            let before: i64 = db
                .query_row("SELECT COUNT(*) FROM storage_fixture", [], |r| r.get(0))
                .unwrap();
            assert_eq!(before, id - 1);
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                team_count
            );
            assert_eq!(
                db.query_row("SELECT cwd FROM teams WHERE id='default'", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                workspace.to_str().unwrap()
            );
            db.execute("INSERT INTO storage_fixture VALUES(?1)", [id])
                .unwrap();
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM storage_fixture", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                id
            );
            assert_eq!(
                std::fs::read(paths.config_dir.join("layout.json")).unwrap(),
                original_layout
            );
            std::fs::create_dir_all(&workspace).unwrap();
            std::fs::write(workspace.join("shell-output"), b"in-place write").unwrap();
        }
        assert_eq!(
            std::fs::read(workspace.join("shell-output")).unwrap(),
            b"in-place write"
        );
        assert!(!data_base.join(IDENTIFIER).exists());
        assert!(!config_base.join(IDENTIFIER).exists());
        assert!(!local_base.exists());
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", repo.join("fixture-global-config"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn selected_legacy_git_worktree_keeps_its_links_and_commits_to_the_same_repository() {
        let root = fixture("worktree");
        let old = root.join(LEGACY_IDENTIFIER);
        let repo = old.join("teams/default/workspace");
        std::fs::create_dir_all(&repo).unwrap();
        crate::db::open(&old.join("dex.db")).unwrap();
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("canary"), "initial").unwrap();
        git(&repo, &["add", "canary"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--no-gpg-sign",
                "-m",
                "initial",
            ],
        );
        let worktree = repo.join(".dex/wt/fixture");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "fixture",
                worktree.to_str().unwrap(),
            ],
        );
        let link = std::fs::read(worktree.join(".git")).unwrap();
        let backlink = repo.join(".git/worktrees/fixture/gitdir");
        let backlink_before = std::fs::read(&backlink).unwrap();
        for _ in 0..2 {
            let selected = select_at(IDENTIFIER, &root, &root, &root);
            assert_eq!(selected.data_dir, old);
            assert_eq!(std::fs::read(worktree.join(".git")).unwrap(), link);
            assert_eq!(std::fs::read(&backlink).unwrap(), backlink_before);
            assert!(!root.join(IDENTIFIER).exists());
        }
        std::fs::write(worktree.join("canary"), "updated").unwrap();
        git(&worktree, &["add", "canary"]);
        git(
            &worktree,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--no-gpg-sign",
                "-m",
                "update",
            ],
        );
        assert_eq!(
            git(&worktree, &["rev-parse", "HEAD"]),
            git(&repo, &["rev-parse", "fixture"])
        );
        git(&repo, &["merge", "--ff-only", "fixture"]);
        assert_eq!(std::fs::read(repo.join("canary")).unwrap(), b"updated");
    }
}
