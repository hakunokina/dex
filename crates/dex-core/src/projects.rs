//! Project records and additive v28 schema. Filesystem/Git inspection is a separate phase.
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) const SCHEMA: &str = r#"

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    root_path TEXT,
    root_key TEXT UNIQUE,
    repo_key TEXT UNIQUE,
    default_branch TEXT,
    worktrees INTEGER NOT NULL DEFAULT 1 CHECK(worktrees IN (0,1)),
    archived_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK ((root_path IS NULL) = (root_key IS NULL))
);
ALTER TABLE teams ADD COLUMN default_project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE teams ADD COLUMN workspace_root TEXT;
ALTER TABLE teams ADD COLUMN team_memory_root TEXT;
ALTER TABLE teams ADD COLUMN agent_memory_root TEXT;
ALTER TABLE delegations ADD COLUMN project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE delegations ADD COLUMN project_root TEXT;
ALTER TABLE delegations ADD COLUMN merge_branch TEXT;
ALTER TABLE tasks ADD COLUMN project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE plan_items ADD COLUMN project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE agent_sessions ADD COLUMN project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE agent_sessions ADD COLUMN delegation_id INTEGER REFERENCES delegations(id) ON DELETE SET NULL;
ALTER TABLE agent_sessions ADD COLUMN cwd TEXT;
ALTER TABLE agent_chain_origins ADD COLUMN project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE agent_chain_origins ADD COLUMN delegation_id INTEGER REFERENCES delegations(id) ON DELETE SET NULL;
ALTER TABLE agent_chain_origins ADD COLUMN cwd TEXT;
ALTER TABLE gpu_requests ADD COLUMN project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE gpu_grants ADD COLUMN project_id TEXT REFERENCES projects(id) DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX IF NOT EXISTS projects_team ON projects(team_id,archived_at,id);
CREATE INDEX IF NOT EXISTS delegations_project ON delegations(project_id,status,id);
CREATE INDEX IF NOT EXISTS delegations_team_project ON delegations(team_id,project_id,status);
CREATE INDEX IF NOT EXISTS tasks_project ON tasks(team_id,project_id,status);
CREATE INDEX IF NOT EXISTS plan_items_project ON plan_items(project_id,plan_id);
CREATE INDEX IF NOT EXISTS agent_sessions_project ON agent_sessions(project_id);
CREATE INDEX IF NOT EXISTS agent_chain_origins_project ON agent_chain_origins(project_id);
CREATE INDEX IF NOT EXISTS gpu_requests_project ON gpu_requests(project_id);
CREATE INDEX IF NOT EXISTS gpu_grants_project ON gpu_grants(project_id);
CREATE TRIGGER IF NOT EXISTS projects_owner_insert BEFORE INSERT ON projects
WHEN NOT EXISTS(SELECT 1 FROM teams WHERE id=NEW.team_id AND kind='main') BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS projects_owner_update BEFORE UPDATE OF team_id ON projects
WHEN NOT EXISTS(SELECT 1 FROM teams WHERE id=NEW.team_id AND kind='main') BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS projects_identity BEFORE UPDATE OF id,team_id ON projects
WHEN NEW.id IS NOT OLD.id OR NEW.team_id IS NOT OLD.team_id BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS teams_project_insert BEFORE INSERT ON teams
WHEN NEW.default_project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.default_project_id AND team_id=NEW.id AND NEW.kind='main') BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS teams_project_update BEFORE UPDATE OF default_project_id,kind ON teams
WHEN NEW.default_project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.default_project_id AND team_id=NEW.id AND NEW.kind='main') BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_project_insert BEFORE INSERT ON delegations
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_project_update BEFORE UPDATE OF project_id,team_id ON delegations
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS tasks_project_insert BEFORE INSERT ON tasks
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS tasks_project_update BEFORE UPDATE OF project_id,team_id ON tasks
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS gpu_requests_project_insert BEFORE INSERT ON gpu_requests
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS gpu_requests_project_update BEFORE UPDATE OF project_id,team_id ON gpu_requests
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS gpu_grants_project_insert BEFORE INSERT ON gpu_grants
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS gpu_grants_project_update BEFORE UPDATE OF project_id,team_id ON gpu_grants
WHEN NEW.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND team_id=NEW.team_id) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_task_project_insert BEFORE INSERT ON delegations
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM tasks WHERE id=NEW.task_id AND (team_id IS NOT NEW.team_id OR (project_id IS NOT NULL AND project_id IS NOT NEW.project_id))) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_task_project_update BEFORE UPDATE OF project_id,task_id ON delegations
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM tasks WHERE id=NEW.task_id AND (team_id IS NOT NEW.team_id OR (project_id IS NOT NULL AND project_id IS NOT NEW.project_id))) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS plan_items_project_insert BEFORE INSERT ON plan_items
WHEN NEW.project_id IS NOT NULL AND (
NOT EXISTS(SELECT 1 FROM projects p JOIN plans n ON n.team_id=p.team_id WHERE p.id=NEW.project_id AND n.id=NEW.plan_id)
OR EXISTS(SELECT 1 FROM tasks t JOIN plans n ON n.id=NEW.plan_id WHERE t.id=NEW.task_id AND (t.team_id IS NOT n.team_id OR (t.project_id IS NOT NULL AND t.project_id IS NOT NEW.project_id)))
OR EXISTS(SELECT 1 FROM delegations d JOIN plans n ON n.id=NEW.plan_id WHERE d.id=NEW.delegation_id AND (d.team_id IS NOT n.team_id OR (d.project_id IS NOT NULL AND d.project_id IS NOT NEW.project_id)))) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS plan_items_project_update BEFORE UPDATE OF project_id,plan_id,task_id,delegation_id ON plan_items
WHEN NEW.project_id IS NOT NULL AND (
NOT EXISTS(SELECT 1 FROM projects p JOIN plans n ON n.team_id=p.team_id WHERE p.id=NEW.project_id AND n.id=NEW.plan_id)
OR EXISTS(SELECT 1 FROM tasks t JOIN plans n ON n.id=NEW.plan_id WHERE t.id=NEW.task_id AND (t.team_id IS NOT n.team_id OR (t.project_id IS NOT NULL AND t.project_id IS NOT NEW.project_id)))
OR EXISTS(SELECT 1 FROM delegations d JOIN plans n ON n.id=NEW.plan_id WHERE d.id=NEW.delegation_id AND (d.team_id IS NOT n.team_id OR (d.project_id IS NOT NULL AND d.project_id IS NOT NEW.project_id)))) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS tasks_links_project_insert BEFORE INSERT ON tasks
WHEN NEW.project_id IS NOT NULL AND (EXISTS(SELECT 1 FROM delegations WHERE task_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)
OR EXISTS(SELECT 1 FROM plan_items WHERE task_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS tasks_links_project_update BEFORE UPDATE OF project_id,team_id ON tasks
WHEN NEW.project_id IS NOT NULL AND (EXISTS(SELECT 1 FROM delegations WHERE task_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)
OR EXISTS(SELECT 1 FROM plan_items WHERE task_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_links_project_insert BEFORE INSERT ON delegations
WHEN NEW.project_id IS NOT NULL AND (EXISTS(SELECT 1 FROM plan_items WHERE delegation_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)
OR EXISTS(SELECT 1 FROM gpu_grants WHERE delegation_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_links_project_update BEFORE UPDATE OF project_id,team_id ON delegations
WHEN NEW.project_id IS NOT NULL AND (EXISTS(SELECT 1 FROM plan_items WHERE delegation_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)
OR EXISTS(SELECT 1 FROM gpu_grants WHERE delegation_id=NEW.id AND project_id IS NOT NULL AND project_id IS NOT NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS gpu_grants_links_project_insert BEFORE INSERT ON gpu_grants
WHEN NEW.project_id IS NOT NULL AND (EXISTS(SELECT 1 FROM delegations WHERE id=NEW.delegation_id AND (team_id IS NOT NEW.team_id OR (project_id IS NOT NULL AND project_id IS NOT NEW.project_id)))
OR EXISTS(SELECT 1 FROM gpu_requests WHERE id=NEW.request_id AND (team_id IS NOT NEW.team_id OR (project_id IS NOT NULL AND project_id IS NOT NEW.project_id)))) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS gpu_grants_links_project_update BEFORE UPDATE OF project_id,delegation_id,request_id ON gpu_grants
WHEN NEW.project_id IS NOT NULL AND (EXISTS(SELECT 1 FROM delegations WHERE id=NEW.delegation_id AND (team_id IS NOT NEW.team_id OR (project_id IS NOT NULL AND project_id IS NOT NEW.project_id)))
OR EXISTS(SELECT 1 FROM gpu_requests WHERE id=NEW.request_id AND (team_id IS NOT NEW.team_id OR (project_id IS NOT NULL AND project_id IS NOT NEW.project_id)))) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS agent_sessions_project_insert BEFORE INSERT ON agent_sessions
WHEN (NEW.delegation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM delegations WHERE id=NEW.delegation_id AND to_agent=NEW.agent_id AND project_id IS NEW.project_id))
OR (NEW.project_id IS NOT NULL AND NEW.delegation_id IS NULL AND NOT EXISTS(SELECT 1 FROM agents a JOIN projects p ON p.team_id=a.team_id WHERE a.id=NEW.agent_id AND p.id=NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS agent_sessions_project_update BEFORE UPDATE OF project_id,delegation_id,agent_id ON agent_sessions
WHEN (NEW.delegation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM delegations WHERE id=NEW.delegation_id AND to_agent=NEW.agent_id AND project_id IS NEW.project_id))
OR (NEW.project_id IS NOT NULL AND NEW.delegation_id IS NULL AND NOT EXISTS(SELECT 1 FROM agents a JOIN projects p ON p.team_id=a.team_id WHERE a.id=NEW.agent_id AND p.id=NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS agent_chain_origins_project_insert BEFORE INSERT ON agent_chain_origins
WHEN (NEW.delegation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM delegations WHERE id=NEW.delegation_id AND to_agent=NEW.agent_id AND project_id IS NEW.project_id))
OR (NEW.project_id IS NOT NULL AND NEW.delegation_id IS NULL AND NOT EXISTS(SELECT 1 FROM agents a JOIN projects p ON p.team_id=a.team_id WHERE a.id=NEW.agent_id AND p.id=NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS agent_chain_origins_project_update BEFORE UPDATE OF project_id,delegation_id,agent_id ON agent_chain_origins
WHEN (NEW.delegation_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM delegations WHERE id=NEW.delegation_id AND to_agent=NEW.agent_id AND project_id IS NEW.project_id))
OR (NEW.project_id IS NOT NULL AND NEW.delegation_id IS NULL AND NOT EXISTS(SELECT 1 FROM agents a JOIN projects p ON p.team_id=a.team_id WHERE a.id=NEW.agent_id AND p.id=NEW.project_id)) BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_project_snapshot BEFORE UPDATE OF project_id,project_root,merge_branch ON delegations
WHEN (OLD.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id)
OR (OLD.project_root IS NOT NULL AND NEW.project_root IS NOT OLD.project_root)
OR (OLD.merge_branch IS NOT NULL AND NEW.merge_branch IS NOT OLD.merge_branch)
BEGIN SELECT RAISE(ABORT,'project_mismatch'); END;
CREATE TRIGGER IF NOT EXISTS delegations_project_root BEFORE INSERT ON delegations
WHEN NEW.project_id IS NOT NULL AND NEW.project_root IS NULL
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND root_path IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS delegations_active_project_insert BEFORE INSERT ON delegations
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS delegations_active_project_update BEFORE UPDATE OF project_id ON delegations
WHEN NEW.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS tasks_active_project_insert BEFORE INSERT ON tasks
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS tasks_active_project_update BEFORE UPDATE OF project_id ON tasks
WHEN NEW.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS plan_items_active_project_insert BEFORE INSERT ON plan_items
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS plan_items_active_project_update BEFORE UPDATE OF project_id ON plan_items
WHEN NEW.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS agent_sessions_active_project_insert BEFORE INSERT ON agent_sessions
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS agent_sessions_active_project_update BEFORE UPDATE OF project_id ON agent_sessions
WHEN NEW.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS agent_chain_origins_active_project_insert BEFORE INSERT ON agent_chain_origins
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS agent_chain_origins_active_project_update BEFORE UPDATE OF project_id ON agent_chain_origins
WHEN NEW.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS gpu_requests_active_project_insert BEFORE INSERT ON gpu_requests
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS gpu_requests_active_project_update BEFORE UPDATE OF project_id ON gpu_requests
WHEN NEW.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS gpu_grants_active_project_insert BEFORE INSERT ON gpu_grants
WHEN NEW.project_id IS NOT NULL AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
CREATE TRIGGER IF NOT EXISTS gpu_grants_active_project_update BEFORE UPDATE OF project_id ON gpu_grants
WHEN NEW.project_id IS NOT NULL AND NEW.project_id IS NOT OLD.project_id
AND EXISTS(SELECT 1 FROM projects WHERE id=NEW.project_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'project_unavailable'); END;
"#;

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Identity key; callers canonicalize existing paths before using it.
/// Missing legacy paths remain unavailable, but reserve their original absolute spelling.
pub fn path_key(path: &str) -> String {
    let path = path
        .strip_prefix(r"\\?\UNC\")
        .map(|p| format!(r"\\{p}"))
        .unwrap_or_else(|| path.strip_prefix(r"\\?\").unwrap_or(path).to_owned());
    #[cfg(windows)]
    let path = path.replace('\\', "/").to_lowercase();
    path.trim_end_matches('/').to_string()
}

/// Stable codes at the new domain boundary; existing APIs keep their error envelopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectError {
    Required,
    NotFound,
    Unavailable,
    Busy,
    RootConflict,
    Mismatch,
    BranchMismatch,
}
impl ProjectError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Required => "project_required",
            Self::NotFound => "project_not_found",
            Self::Unavailable => "project_unavailable",
            Self::Busy => "project_busy",
            Self::RootConflict => "project_root_conflict",
            Self::Mismatch => "project_mismatch",
            Self::BranchMismatch => "project_branch_mismatch",
        }
    }
    /// #838 G2: an error string of the project APIs back to its code (RPC `{code,message}`).
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::Required,
            Self::NotFound,
            Self::Unavailable,
            Self::Busy,
            Self::RootConflict,
            Self::Mismatch,
            Self::BranchMismatch,
        ]
        .into_iter()
        .find(|e| e.code() == code)
    }
}
impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ProjectError {}
impl From<rusqlite::Error> for ProjectError {
    fn from(e: rusqlite::Error) -> Self {
        // Do not expose DB messages (UNIQUE values can contain private paths).
        match &e {
            rusqlite::Error::SqliteFailure(_, Some(m))
                if m.contains("projects.root_key") || m.contains("projects.repo_key") =>
            {
                Self::RootConflict
            }
            rusqlite::Error::SqliteFailure(_, Some(m)) if m == "project_mismatch" => Self::Mismatch,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub root_path: Option<String>,
    pub root_key: Option<String>,
    pub repo_key: Option<String>,
    pub default_branch: Option<String>,
    pub worktrees: bool,
    pub archived_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub worktree_cleanup: i64,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProject {
    pub name: String,
    pub worktrees: Option<bool>,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPatch {
    pub name: Option<String>,
    pub worktrees: Option<bool>,
}

/// Prepared outside the Db mutex, then applied under the mutation/start gate.
/// Private fields prevent IPC callers from supplying a forged canonical identity.
#[derive(Debug, Clone)]
pub struct PreparedRoot {
    pub(crate) root_path: String,
    pub(crate) root_key: String,
    pub(crate) repo_key: Option<String>,
    pub(crate) default_branch: Option<String>,
    pub(crate) head_branch: Option<String>,
}
impl PreparedRoot {
    pub fn root_path(&self) -> &str {
        &self.root_path
    }
    pub fn repo_key(&self) -> Option<&str> {
        self.repo_key.as_deref()
    }
    pub fn default_branch(&self) -> Option<&str> {
        self.default_branch.as_deref()
    }
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output, ProjectError> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(key);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd.output().map_err(|_| ProjectError::Unavailable)
}
fn output_text(output: std::process::Output) -> Result<String, ProjectError> {
    if !output.status.success() {
        return Err(ProjectError::Unavailable);
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|_| ProjectError::Unavailable)
}

/// Canonical root and Git common-dir ownership, without git init/checkout.
/// A repository subdirectory is rejected: the selected folder must be its toplevel.
pub fn prepare_root(raw: &str, branch: Option<&str>) -> Result<PreparedRoot, ProjectError> {
    let root_path = crate::teams::normalize_dir(raw).map_err(|_| ProjectError::Unavailable)?;
    let root = Path::new(&root_path);
    let root_key = path_key(&root_path);
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    if !top.status.success() {
        if root.join(".git").exists()
            || root.join("HEAD").is_file() && root.join("objects").is_dir()
        {
            return Err(ProjectError::Unavailable);
        }
        if branch.is_some() {
            return Err(ProjectError::BranchMismatch);
        }
        return Ok(PreparedRoot {
            root_path,
            root_key,
            repo_key: None,
            default_branch: None,
            head_branch: None,
        });
    }
    let top =
        crate::teams::normalize_dir(&output_text(top)?).map_err(|_| ProjectError::Unavailable)?;
    if path_key(&top) != root_key {
        return Err(ProjectError::Mismatch);
    }
    let common = output_text(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)?;
    let common = crate::teams::normalize_dir(&common).map_err(|_| ProjectError::Unavailable)?;
    let head = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let head_branch = if head.status.success() {
        Some(output_text(head)?)
    } else {
        None
    };
    let default_branch = branch.map(str::to_string).or_else(|| head_branch.clone());
    if let Some(b) = default_branch.as_deref() {
        if !git(root, &["check-ref-format", "--branch", b])?
            .status
            .success()
        {
            return Err(ProjectError::BranchMismatch);
        }
        let exists = git(
            root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )?
        .status
        .success();
        // An unborn repository may retain its symbolic branch; it has no merge/worktree yet.
        let unborn = !git(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?
            .status
            .success();
        if !(exists || unborn && head_branch.as_deref() == Some(b)) {
            return Err(ProjectError::BranchMismatch);
        }
    } else {
        return Err(ProjectError::BranchMismatch);
    }
    Ok(PreparedRoot {
        root_path,
        root_key,
        repo_key: Some(path_key(&common)),
        default_branch,
        head_branch,
    })
}

const COLS: &str="p.id,p.team_id,p.name,p.root_path,p.root_key,p.repo_key,p.default_branch,p.worktrees,p.archived_at,p.created_at,p.updated_at,COALESCE(w.cleanup,0)";
const FROM: &str = "projects p LEFT JOIN (
    SELECT d.project_id,COUNT(*) AS cleanup FROM delegation_worktrees w
    JOIN delegations d ON d.id=w.delegation_id WHERE w.removed_at IS NULL
    GROUP BY d.project_id
) w ON w.project_id=p.id";
fn row(r: &Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        team_id: r.get(1)?,
        name: r.get(2)?,
        root_path: r.get(3)?,
        root_key: r.get(4)?,
        repo_key: r.get(5)?,
        default_branch: r.get(6)?,
        worktrees: r.get(7)?,
        archived_at: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        worktree_cleanup: r.get(11)?,
    })
}
pub fn list(
    conn: &Connection,
    team: &str,
    include_archived: bool,
) -> Result<Vec<Project>, ProjectError> {
    let mut st=conn.prepare(&format!("SELECT {COLS} FROM {FROM} WHERE p.team_id=?1 AND (?2 OR p.archived_at IS NULL) ORDER BY p.created_at,p.id"))?;
    let rows = st.query_map(params![team, include_archived], row)?;
    rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
}
pub fn get(conn: &Connection, team: &str, id: &str) -> Result<Project, ProjectError> {
    conn.query_row(
        &format!("SELECT {COLS} FROM {FROM} WHERE p.team_id=?1 AND p.id=?2"),
        params![team, id],
        row,
    )
    .optional()?
    .ok_or(ProjectError::NotFound)
}
fn check_name(conn: &Connection, team: &str, id: &str, name: &str) -> Result<String, ProjectError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ProjectError::Mismatch);
    }
    if list(conn, team, false)?
        .iter()
        .any(|p| p.id != id && p.name.to_lowercase() == name.to_lowercase())
    {
        return Err(ProjectError::Mismatch);
    }
    Ok(name.to_owned())
}
fn nested(a: &str, b: &str) -> bool {
    a == b || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
}
fn check_root(conn: &Connection, id: &str, ready: &PreparedRoot) -> Result<(), ProjectError> {
    let mut st = conn.prepare(
        "SELECT id,root_key,repo_key,archived_at FROM projects WHERE root_key IS NOT NULL",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })?;
    for r in rows {
        let (other, root, repo, archived) = r?;
        if other == id {
            continue;
        }
        if ready.root_key == root
            || ready.repo_key.is_some() && ready.repo_key == repo
            || archived.is_none()
                && (nested(&ready.root_key, &root) || nested(&root, &ready.root_key))
        {
            return Err(ProjectError::RootConflict);
        }
    }
    Ok(())
}
pub(crate) fn reserve_root(conn: &Connection, root: &str) -> Result<(), ProjectError> {
    check_root(
        conn,
        "",
        &PreparedRoot {
            root_path: root.into(),
            root_key: path_key(root),
            repo_key: None,
            default_branch: None,
            head_branch: None,
        },
    )
}
pub fn create(
    conn: &Connection,
    team: &str,
    input: NewProject,
    ready: &PreparedRoot,
) -> Result<Project, ProjectError> {
    let tx = conn.unchecked_transaction()?;
    if !tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM teams WHERE id=?1 AND kind='main')",
        [team],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(ProjectError::NotFound);
    }
    check_root(&tx, "", ready)?;
    let name = check_name(&tx, team, "", &input.name)?;
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|_| ProjectError::Unavailable)?;
    let id = format!(
        "project-{}",
        raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let ts = now();
    tx.execute("INSERT INTO projects(id,team_id,name,root_path,root_key,repo_key,default_branch,worktrees,created_at,updated_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?9)",params![id,team,name,ready.root_path,ready.root_key,ready.repo_key,ready.default_branch,input.worktrees.unwrap_or(true),ts])?;
    let p = get(&tx, team, &id)?;
    tx.commit()?;
    Ok(p)
}

/// Caller holds the same gate used for spawning/mutating projects; live must consult
/// the PTY launch/session project binding, never the next queued job or UI default.
pub fn ensure_idle(
    conn: &Connection,
    team: &str,
    id: &str,
    live: impl Fn(&str) -> bool,
) -> Result<(), ProjectError> {
    get(conn, team, id)?;
    if live(id) {
        return Err(ProjectError::Busy);
    }
    let busy=conn.query_row("SELECT
        EXISTS(SELECT 1 FROM delegations WHERE (project_id=?1 OR project_id IS NULL AND team_id=?2) AND status IN ('queued','running'))
        OR EXISTS(SELECT 1 FROM plan_items i JOIN plans p ON p.id=i.plan_id WHERE (i.project_id=?1 OR i.project_id IS NULL AND p.team_id=?2) AND p.status='active' AND i.status NOT IN ('done','cancelled'))
        OR EXISTS(SELECT 1 FROM delegation_reviews r LEFT JOIN delegations d ON d.id=r.target_id LEFT JOIN delegations v ON v.id=r.review_id WHERE (d.project_id=?1 OR v.project_id=?1 OR d.project_id IS NULL AND d.team_id=?2 OR v.project_id IS NULL AND v.team_id=?2) AND r.verdict IS NULL)
        OR EXISTS(SELECT 1 FROM delegation_worktrees w JOIN delegations d ON d.id=w.delegation_id WHERE (d.project_id=?1 OR d.project_id IS NULL AND d.team_id=?2) AND w.removed_at IS NULL)",
        params![id,team],|r|r.get::<_,bool>(0))?;
    if busy {
        Err(ProjectError::Busy)
    } else {
        Ok(())
    }
}
pub fn update(
    conn: &Connection,
    team: &str,
    id: &str,
    patch: ProjectPatch,
    ready: Option<&PreparedRoot>,
    live: impl Fn(&str) -> bool,
) -> Result<Project, ProjectError> {
    let tx = conn.unchecked_transaction()?;
    let p = update_in(&tx, team, id, patch, ready, live)?;
    tx.commit()?;
    Ok(p)
}
pub(crate) fn update_in(
    conn: &Connection,
    team: &str,
    id: &str,
    patch: ProjectPatch,
    ready: Option<&PreparedRoot>,
    live: impl Fn(&str) -> bool,
) -> Result<Project, ProjectError> {
    let p = get(conn, team, id)?;
    if ready.is_some() || patch.worktrees.is_some() {
        ensure_idle(conn, team, id, live)?;
    }
    let name = match patch.name {
        Some(ref name) => check_name(conn, team, id, name)?,
        None => p.name,
    };
    if let Some(root) = ready {
        check_root(conn, id, root)?;
        conn.execute("UPDATE projects SET root_path=?2,root_key=?3,repo_key=?4,default_branch=?5 WHERE id=?1",
            params![id,root.root_path,root.root_key,root.repo_key,root.default_branch])?;
    }
    conn.execute(
        "UPDATE projects SET name=?2,worktrees=?3,updated_at=?4 WHERE id=?1",
        params![id, name, patch.worktrees.unwrap_or(p.worktrees), now()],
    )?;
    sync_legacy_view(conn, team)?;
    get(conn, team, id)
}
pub(crate) fn update_legacy_root(
    conn: &Connection,
    team: &str,
    id: &str,
    root: &str,
) -> Result<(), ProjectError> {
    let ready = PreparedRoot {
        root_path: root.into(),
        root_key: path_key(root),
        repo_key: None,
        default_branch: None,
        head_branch: None,
    };
    update_in(
        conn,
        team,
        id,
        ProjectPatch::default(),
        Some(&ready),
        |_| false,
    )
    .map(|_| ())
}
fn sync_legacy_view(conn: &Connection, team: &str) -> Result<(), ProjectError> {
    conn.execute("UPDATE teams SET cwd=(SELECT root_path FROM projects WHERE id=teams.default_project_id),
        worktrees=COALESCE((SELECT worktrees FROM projects WHERE id=teams.default_project_id),worktrees)
        WHERE id=?1 AND kind='main'",[team])?;
    Ok(())
}
pub fn archive(
    conn: &Connection,
    team: &str,
    id: &str,
    live: impl Fn(&str) -> bool,
) -> Result<Project, ProjectError> {
    let tx = conn.unchecked_transaction()?;
    ensure_idle(&tx, team, id, live)?;
    tx.execute(
        "UPDATE projects SET archived_at=COALESCE(archived_at,?2),updated_at=?2 WHERE id=?1",
        params![id, now()],
    )?;
    let p = get(&tx, team, id)?;
    tx.commit()?;
    Ok(p)
}
pub fn reopen(conn: &Connection, team: &str, id: &str) -> Result<Project, ProjectError> {
    let tx = conn.unchecked_transaction()?;
    let p = get(&tx, team, id)?;
    check_name(&tx, team, id, &p.name)?;
    if let Some(root) = p.root_path.as_ref() {
        check_root(
            &tx,
            id,
            &PreparedRoot {
                root_path: root.clone(),
                root_key: p.root_key.clone().ok_or(ProjectError::Unavailable)?,
                repo_key: p.repo_key,
                default_branch: p.default_branch,
                head_branch: None,
            },
        )?;
    }
    tx.execute(
        "UPDATE projects SET archived_at=NULL,updated_at=?2 WHERE id=?1",
        params![id, now()],
    )?;
    let p = get(&tx, team, id)?;
    tx.commit()?;
    Ok(p)
}
/// Explicit leader/UI selection. It never rewrites job/item/session snapshots.
pub fn set_default(conn: &Connection, team: &str, id: Option<&str>) -> Result<(), ProjectError> {
    if let Some(id) = id {
        if get(conn, team, id)?.archived_at.is_some() {
            return Err(ProjectError::Unavailable);
        }
    }
    if conn.execute(
        "UPDATE teams SET default_project_id=?2,
        cwd=(SELECT root_path FROM projects WHERE id=?2),
        worktrees=COALESCE((SELECT worktrees FROM projects WHERE id=?2),worktrees)
        WHERE id=?1 AND kind='main'",
        params![team, id],
    )? == 0
    {
        return Err(ProjectError::NotFound);
    }
    Ok(())
}

/// Revalidate the prepared metadata against the DB snapshot. This does not change the root;
/// an explicit move uses update. Run before first job/merge, outside listing/render paths.
pub fn verify(
    conn: &Connection,
    team: &str,
    id: &str,
    ready: &PreparedRoot,
) -> Result<Project, ProjectError> {
    let p = get(conn, team, id)?;
    if p.archived_at.is_some() || p.root_key.as_deref() != Some(&ready.root_key) {
        return Err(ProjectError::Unavailable);
    }
    if p.repo_key.is_some() && p.repo_key != ready.repo_key {
        return Err(ProjectError::RootConflict);
    }
    if p.default_branch.is_some() && p.default_branch != ready.default_branch {
        return Err(ProjectError::BranchMismatch);
    }
    check_root(conn, id, ready)?;
    conn.execute("UPDATE projects SET repo_key=COALESCE(repo_key,?2),default_branch=COALESCE(default_branch,?3) WHERE id=?1",params![id,ready.repo_key,ready.default_branch])?;
    get(conn, team, id)
}

/// Called at startup before a Db mutex exists. Never moves any memory files.
pub fn backfill(conn: &Connection, data_dir: &Path) -> rusqlite::Result<()> {
    let mut st = conn.prepare("SELECT id,cwd,worktrees,created_at FROM teams WHERE kind='main'")?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    // Disk inspection is outside the write transaction (and outside the Core's mutex).
    let roots: Vec<_> = rows
        .iter()
        .map(|(_, cwd, _, _)| {
            cwd.as_ref()
                .map(|p| crate::teams::normalize_dir(p).unwrap_or_else(|_| p.clone()))
        })
        .collect();
    let tx = conn.unchecked_transaction()?;
    for ((id, cwd, worktrees, created), root) in rows.iter().zip(roots) {
        let memory_anchor = cwd
            .as_ref()
            .map(|p| Path::new(p).join(".dex").join("memory"));
        backfill_team(
            &tx,
            data_dir,
            id,
            root.as_deref(),
            *worktrees,
            *created,
            memory_anchor.as_deref(),
        )?;
    }
    let mut st = tx.prepare("SELECT id FROM teams WHERE kind='service'")?;
    let services = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    for id in services {
        backfill_service(&tx, data_dir, &id)?;
    }
    // Only pre-v28 rows are legacy. Reopening must not silently route newly unbound tasks/jobs.
    if !tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key='projects_backfilled')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        fill_links(&tx)?;
        tx.execute(
            "INSERT INTO settings(key,value) VALUES('projects_backfilled','true')",
            [],
        )?;
    }
    tx.commit()
}

pub(crate) fn backfill_service(
    conn: &Connection,
    data_dir: &Path,
    id: &str,
) -> rusqlite::Result<()> {
    let base = crate::teams::service_dir(data_dir, id);
    conn.execute("UPDATE teams SET workspace_root=COALESCE(workspace_root,?2),
        team_memory_root=COALESCE(team_memory_root,?3),agent_memory_root=COALESCE(agent_memory_root,?3) WHERE id=?1",
        params![id,base.join("workspace").to_string_lossy(),base.join("memory").to_string_lossy()])?;
    Ok(())
}

/// DB-only half, also used inside the new-team transaction. Git verification is deferred
/// to prepare/verify before first use. New-team memory uses app-data; legacy anchors stay put.
pub(crate) fn backfill_team(
    conn: &Connection,
    data_dir: &Path,
    id: &str,
    root: Option<&str>,
    worktrees: bool,
    created: i64,
    memory_anchor: Option<&Path>,
) -> rusqlite::Result<()> {
    let base = crate::teams::service_dir(data_dir, id);
    let memory = memory_anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| base.join("memory"));
    let inserted = conn.execute(
        "INSERT INTO projects(id,team_id,name,root_path,root_key,worktrees,created_at,updated_at)
        VALUES(?1,?2,'Varsayılan proje',?3,?4,?5,?6,?6) ON CONFLICT(id) DO NOTHING",
        params![
            format!("legacy-{id}"),
            id,
            root,
            root.map(path_key),
            worktrees,
            created
        ],
    )?;
    conn.execute("UPDATE teams SET default_project_id=CASE WHEN ?5 THEN COALESCE(default_project_id,?2) ELSE default_project_id END,
        workspace_root=COALESCE(workspace_root,?3),team_memory_root=COALESCE(team_memory_root,?4),
        agent_memory_root=COALESCE(agent_memory_root,?4) WHERE id=?1",
        params![id,format!("legacy-{id}"),base.join("workspace").to_string_lossy(),memory.to_string_lossy(),inserted!=0])?;
    Ok(())
}

fn fill_links(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("
        UPDATE tasks SET project_id=(SELECT default_project_id FROM teams WHERE id=tasks.team_id AND kind='main')
            WHERE project_id IS NULL;
        UPDATE delegations SET
            project_id=COALESCE(project_id,(SELECT default_project_id FROM teams WHERE id=delegations.team_id AND kind='main')),
            project_root=COALESCE(project_root,(SELECT root FROM delegation_worktrees WHERE delegation_id=delegations.id))
            WHERE project_id IS NULL OR project_root IS NULL;
        UPDATE plan_items SET project_id=(SELECT t.default_project_id FROM plans p JOIN teams t ON t.id=p.team_id AND t.kind='main' WHERE p.id=plan_items.plan_id)
            WHERE project_id IS NULL;
        UPDATE gpu_requests SET project_id=(SELECT default_project_id FROM teams WHERE id=gpu_requests.team_id AND kind='main')
            WHERE project_id IS NULL;
        UPDATE gpu_grants SET project_id=COALESCE(
            (SELECT project_id FROM delegations WHERE id=gpu_grants.delegation_id),
            (SELECT default_project_id FROM teams WHERE id=gpu_grants.team_id AND kind='main'))
            WHERE project_id IS NULL;
    ")?;
    let mut st=conn.prepare("SELECT id,result_path FROM delegations WHERE project_id IS NOT NULL AND project_root IS NULL AND result_path IS NOT NULL")?;
    let jobs = st
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    for (id, report) in jobs {
        if let Some(root) = historical_report_root(id, &report) {
            conn.execute(
                "UPDATE delegations SET project_root=?2 WHERE id=?1 AND project_root IS NULL",
                params![id, root.to_string_lossy()],
            )?;
        }
    }
    Ok(())
}

fn historical_report_root(id: i64, report: &str) -> Option<PathBuf> {
    let path = Path::new(report);
    if !path.is_absolute() {
        return None;
    }
    let parent = path.parent()?;
    let scope = parent.parent()?;
    let filename = path.file_name()?.to_str()?;
    let standard = parent.file_name()? == "results"
        && scope.file_name()? == ".dex"
        && filename == format!("{id}.md");
    let service = parent.file_name()? == "research"
        && scope.file_name()? == "docs"
        && filename.starts_with(&format!("{id}-"))
        && filename.ends_with(".md");
    (standard || service)
        .then(|| scope.parent().map(Path::to_path_buf))
        .flatten()
}

/// #838 G2: an agent's saved session binding (`agent_sessions`) — what its session runs on,
/// never the next queued job.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProject {
    pub project_id: Option<String>,
    pub delegation_id: Option<i64>,
}

fn code(e: ProjectError) -> String {
    e.to_string()
}

/// #838 G2: the patron's project calls, one body for the in-app commands and dexd. Errors are
/// the stable codes (`project_busy`, …). Filesystem/Git work runs before any lock; mutations
/// then run under the start gate (a spawn's bind cannot interleave) and one DB lock.
impl crate::Core {
    pub fn project_list(&self, team_id: &str, include_archived: bool) -> Result<Vec<Project>, String> {
        crate::db::with(&self.db, |c| list(c, team_id, include_archived).map_err(code))
    }

    pub fn project_create(&self, team_id: &str, input: NewProject, root: &str, branch: Option<&str>) -> Result<Project, String> {
        let ready = prepare_root(root, branch).map_err(code)?;
        let p = self.project_gate(|c, _| create(c, team_id, input, &ready))?;
        self.projects_changed(team_id, false);
        Ok(p)
    }

    pub fn project_create_folder(&self, team_id: &str, input: NewProject, branch: Option<&str>) -> Result<Project, String> {
        let team = crate::db::with(&self.db, |c| crate::teams::get_team(c, &self.paths.data_dir, team_id))?;
        if team.kind != "main" {
            return Err("project_not_found".into());
        }
        let workspace = team.workspace_root.ok_or("project_unavailable")?;
        let workspace = PathBuf::from(crate::teams::normalize_dir(&workspace).map_err(|_| "project_unavailable")?);
        let name = crate::teams::folder_name(&input.name)?;
        let root = workspace.join(name);
        match std::fs::create_dir(&root) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err("project_unavailable".into()),
        }
        let root = crate::teams::normalize_dir(&root.to_string_lossy()).map_err(|_| "project_unavailable")?;
        if !Path::new(&root).starts_with(&workspace) {
            return Err("project_root_conflict".into());
        }
        let ready = prepare_root(&root, branch).map_err(code)?;
        let p = self.project_gate(|c, _| {
            let current = crate::teams::get_team(c, &self.paths.data_dir, team_id).map_err(|_| ProjectError::NotFound)?;
            if current.workspace_root.as_deref().map(path_key) != Some(path_key(&workspace.to_string_lossy())) {
                return Err(ProjectError::Mismatch);
            }
            create(c, team_id, input, &ready)
        })?;
        self.projects_changed(team_id, false);
        Ok(p)
    }

    /// `root`/`branch` re-bind the folder or merge branch (busy check applies); a branch alone
    /// is checked on the current root, a root alone takes its checked-out branch.
    pub fn project_update(
        &self,
        team_id: &str,
        id: &str,
        patch: ProjectPatch,
        root: Option<&str>,
        branch: Option<&str>,
    ) -> Result<Project, String> {
        let ready = match (root, branch) {
            (None, None) => None,
            (Some(root), branch) => Some(prepare_root(root, branch).map_err(code)?),
            (None, Some(branch)) => {
                let current = crate::db::with(&self.db, |c| get(c, team_id, id).map_err(code))?;
                let root = current.root_path.ok_or_else(|| code(ProjectError::Unavailable))?;
                Some(prepare_root(&root, Some(branch)).map_err(code)?)
            }
        };
        let p = self.project_gate(|c, live| update(c, team_id, id, patch, ready.as_ref(), live))?;
        self.projects_changed(team_id, true);
        Ok(p)
    }

    pub fn project_archive(&self, team_id: &str, id: &str) -> Result<Project, String> {
        let p = self.project_gate(|c, live| archive(c, team_id, id, live))?;
        self.projects_changed(team_id, false);
        Ok(p)
    }

    pub fn project_reopen(&self, team_id: &str, id: &str) -> Result<Project, String> {
        let p = self.project_gate(|c, _| reopen(c, team_id, id))?;
        self.projects_changed(team_id, false);
        Ok(p)
    }

    /// The team's selected project (`None` clears it): new terminals and the leader's
    /// single-project view; jobs, plan items and sessions keep their own binding.
    pub fn project_set_default(&self, team_id: &str, id: Option<&str>) -> Result<(), String> {
        self.project_gate(|c, _| set_default(c, team_id, id))?;
        self.projects_changed(team_id, true);
        Ok(())
    }

    /// The team's context (no agent): the given project, else its single bound one; several
    /// bound and none given → the team workspace without a project. DB only.
    pub fn project_context(&self, team_id: &str, project_id: Option<&str>) -> Result<crate::project_context::ProjectContext, String> {
        crate::db::with(&self.db, |c| self.team_context_in(c, team_id, project_id))
    }

    /// Checks the project's folder and Git identity now (not on listing): the project, or
    /// why it cannot take work. A folderless legacy project has nothing to check.
    pub fn project_verify(&self, team_id: &str, id: &str) -> Result<Project, String> {
        let ctx = crate::db::with(&self.db, |c| self.team_context_in(c, team_id, Some(id)))?;
        let ready = crate::project_context::validate(&ctx).map_err(code)?;
        let (before, after) = self.project_gate(|c, _| {
            let before = get(c, team_id, id)?;
            let after = match ready.as_ref() {
                Some(ready) => verify(c, team_id, id, ready)?,
                None => before.clone(),
            };
            Ok((before, after))
        })?;
        if (&before.repo_key, &before.default_branch) != (&after.repo_key, &after.default_branch) {
            self.projects_changed(team_id, false);
        }
        Ok(after)
    }

    /// Bound sessions by agent (`team_id` narrows to one team); unbound ones are absent.
    pub fn session_projects(&self, team_id: Option<&str>) -> Result<std::collections::BTreeMap<String, SessionProject>, String> {
        crate::db::with(&self.db, |c| {
            let mut st = c
                .prepare(
                    "SELECT s.agent_id,COALESCE(s.project_id,d.project_id),s.delegation_id FROM agent_sessions s
                    JOIN agents a ON a.id=s.agent_id LEFT JOIN delegations d ON d.id=s.delegation_id
                    WHERE (?1 IS NULL OR a.team_id=?1) AND (s.project_id IS NOT NULL OR s.delegation_id IS NOT NULL)",
                )
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map([team_id], |r| {
                    Ok((r.get::<_, String>(0)?, SessionProject { project_id: r.get(1)?, delegation_id: r.get(2)? }))
                })
                .map_err(|e| e.to_string())?;
            let mut out = std::collections::BTreeMap::new();
            for row in rows {
                let (agent, p) = row.map_err(|e| e.to_string())?;
                out.insert(agent, p);
            }
            Ok(out)
        })
    }

    fn team_context_in(&self, c: &Connection, team_id: &str, project_id: Option<&str>) -> Result<crate::project_context::ProjectContext, String> {
        use crate::project_context::{resolve, ContextMode, ResolveRequest};
        let request = ResolveRequest { team_id, agent_id: None, delegation_id: None, project_id, mode: ContextMode::Team };
        resolve(c, &self.paths.data_dir, request).map_err(code)
    }

    /// `f` under the start gate and the DB lock; `live(project)` = a running PTY whose session
    /// is bound to it (G1's contract: never the queue or the UI default).
    fn project_gate<T>(&self, f: impl FnOnce(&Connection, &dyn Fn(&str) -> bool) -> Result<T, ProjectError>) -> Result<T, String> {
        let _gate = self.agent_start_gate.lock().map_err(|_| "başlatma kilidi zehirli")?;
        crate::db::with(&self.db, |c| {
            let mut st = c
                .prepare(
                    "SELECT s.agent_id,COALESCE(s.project_id,d.project_id) FROM agent_sessions s
                    LEFT JOIN delegations d ON d.id=s.delegation_id WHERE COALESCE(s.project_id,d.project_id) IS NOT NULL",
                )
                .map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            let mut live = std::collections::HashSet::new();
            for row in rows {
                let (agent, project) = row.map_err(|e| e.to_string())?;
                if self.ptys.is_running(&agent) {
                    live.insert(project);
                }
            }
            f(c, &|id| live.contains(id)).map_err(code)
        })
    }

    fn projects_changed(&self, team_id: &str, team_view: bool) {
        let _ = self.events.emit("projects-changed", &serde_json::json!({ "teamId": team_id }));
        // set_default/update mirror the default project into the legacy `teams.cwd/worktrees`.
        if team_view {
            let _ = self.events.emit("teams-changed", &());
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct Fixture(pub PathBuf);
    impl Fixture {
        pub(crate) fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/projects-844");
            std::fs::create_dir_all(&base).unwrap();
            let base = PathBuf::from(crate::teams::normalize_dir(base.to_str().unwrap()).unwrap());
            let path = base.join(format!(
                "{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        pub(crate) fn dir(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir(&path).unwrap();
            // Isolate this fixture from the enclosing worktree repository.
            assert!(
                git(&path, &["init", "--quiet", "--initial-branch=fixture-main"])
                    .unwrap()
                    .status
                    .success()
            );
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn ownership_crud_and_explicit_default_keep_job_snapshot() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let root = f.dir("p1");
        let ready = prepare_root(root.to_str().unwrap(), None).unwrap();
        let p = create(
            &c,
            "default",
            NewProject {
                name: "App".into(),
                worktrees: None,
            },
            &ready,
        )
        .unwrap();
        let err = create(
            &c,
            "default",
            NewProject {
                name: "Other".into(),
                worktrees: None,
            },
            &ready,
        )
        .unwrap_err();
        assert_eq!(err.code(), "project_root_conflict");
        assert_eq!(
            get(&c, "foreign", &p.id).unwrap_err().code(),
            "project_not_found"
        );
        set_default(&c, "default", Some(&p.id)).unwrap();
        c.execute("INSERT INTO delegations(from_agent,to_agent,team_id,prompt,status,project_id,project_root,created_at,updated_at) VALUES('timur','kaan','default','x','done',?1,?2,1,1)",params![p.id,p.root_path]).unwrap();
        let id = c.last_insert_rowid();
        set_default(&c, "default", Some("legacy-default")).unwrap();
        assert_eq!(
            c.query_row(
                "SELECT project_id FROM delegations WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            p.id
        );
        let p = update(
            &c,
            "default",
            &p.id,
            ProjectPatch {
                name: Some("Renamed".into()),
                worktrees: Some(false),
            },
            None,
            |_| false,
        )
        .unwrap();
        assert_eq!((p.name.as_str(), p.worktrees), ("Renamed", false));
        assert!(archive(&c, "default", &p.id, |_| true).is_err());
        let archived = archive(&c, "default", &p.id, |_| false).unwrap();
        assert!(archived.archived_at.is_some());
        assert_eq!(reopen(&c, "default", &p.id).unwrap().id, p.id);
    }

    #[test]
    fn archive_gates_cover_unbound_legacy_jobs_plan_review_and_all_worktrees() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let root = f.dir("root");
        let ready = prepare_root(root.to_str().unwrap(), None).unwrap();
        let p = create(
            &c,
            "default",
            NewProject {
                name: "App".into(),
                worktrees: None,
            },
            &ready,
        )
        .unwrap();
        c.execute_batch("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,created_at,updated_at) VALUES(1,'timur','kaan','default','x',1,1)").unwrap();
        assert_eq!(
            archive(&c, "default", &p.id, |_| false).unwrap_err().code(),
            "project_busy"
        );
        c.execute(
            "UPDATE delegations SET status='done',project_id=?1,project_root=?2 WHERE id=1",
            params![p.id, p.root_path],
        )
        .unwrap();
        c.execute_batch("INSERT INTO plans(id,team_id,leader,created_at,updated_at) VALUES(1,'default','timur',1,1)").unwrap();
        c.execute("INSERT INTO plan_items(id,plan_id,key,title,prompt,target,project_id) VALUES(1,1,'a','a','x','backend',?1)",[&p.id]).unwrap();
        assert_eq!(
            archive(&c, "default", &p.id, |_| false).unwrap_err().code(),
            "project_busy"
        );
        c.execute("UPDATE plan_items SET status='done' WHERE id=1", [])
            .unwrap();
        c.execute_batch("INSERT INTO delegation_reviews(review_id,target_id,sha,created_at) VALUES(1,999,'sha',1)").unwrap();
        assert_eq!(
            archive(&c, "default", &p.id, |_| false).unwrap_err().code(),
            "project_busy"
        );
        c.execute(
            "UPDATE delegation_reviews SET verdict='pass' WHERE review_id=1",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO delegation_worktrees(delegation_id,agent_id,root,path,branch,base,created_at,merged_at) VALUES(1,'kaan',?1,'path','branch','base',1,2)",[&p.root_path]).unwrap();
        assert_eq!(
            archive(&c, "default", &p.id, |_| false).unwrap_err().code(),
            "project_busy"
        );
        assert_eq!(
            list(&c, "default", false)
                .unwrap()
                .iter()
                .find(|v| v.id == p.id)
                .unwrap()
                .worktree_cleanup,
            1
        );
        c.execute(
            "UPDATE delegation_worktrees SET removed_at=3 WHERE delegation_id=1",
            [],
        )
        .unwrap();
        assert!(archive(&c, "default", &p.id, |_| false).is_ok());
    }

    #[test]
    fn historical_report_is_evidence_but_current_cwd_is_not() {
        let f = Fixture::new();
        let file = f.0.join("dex.db");
        // Build a legacy host before its first project backfill.
        let c = crate::db::open_bare(&file).unwrap();
        c.execute("DELETE FROM settings WHERE key='projects_backfilled'", [])
            .unwrap();
        let current = f.dir("current");
        c.execute(
            "INSERT INTO teams(id,name,kind,cwd,created_at) VALUES('old','Old','main',?1,1)",
            [current.to_str()],
        )
        .unwrap();
        let historical = f.0.join("historical");
        let report = historical.join(".dex/results/1.md");
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,result_path,created_at,updated_at) VALUES(1,'l','w','old','x','done',?1,1,1),(2,'l','w','old','x','done',NULL,1,1)",[report.to_str()]).unwrap();
        backfill(&c, &f.0).unwrap();
        assert_eq!(
            c.query_row("SELECT project_root FROM delegations WHERE id=1", [], |r| r
                .get::<_, Option<String>>(0))
                .unwrap(),
            Some(historical.to_string_lossy().into())
        );
        assert!(c
            .query_row("SELECT project_root FROM delegations WHERE id=2", [], |r| r
                .get::<_, Option<String>>(0))
            .unwrap()
            .is_none());
        c.execute(
            "INSERT INTO tasks(title,team_id,created_at,updated_at) VALUES('new','old',2,2)",
            [],
        )
        .unwrap();
        backfill(&c, &f.0).unwrap();
        assert!(
            c.query_row("SELECT project_id FROM tasks WHERE title='new'", [], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap()
            .is_none(),
            "reopen cannot silently route new unbound tasks"
        );
    }

    #[test]
    fn canonical_repo_and_path_ownership_are_unique_and_nested_roots_rejected() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let root = f.dir("repo");
        std::fs::write(root.join("a.txt"), "base").unwrap();
        assert!(git(&root, &["add", "a.txt"]).unwrap().status.success());
        assert!(git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "base"
            ]
        )
        .unwrap()
        .status
        .success());
        let ready = prepare_root(root.to_str().unwrap(), None).unwrap();
        let p = create(
            &c,
            "default",
            NewProject {
                name: "App".into(),
                worktrees: None,
            },
            &ready,
        )
        .unwrap();
        let alias = prepare_root(root.join(".").to_str().unwrap(), None).unwrap();
        assert_eq!(
            create(
                &c,
                "default",
                NewProject {
                    name: "Alias".into(),
                    worktrees: None
                },
                &alias
            )
            .unwrap_err()
            .code(),
            "project_root_conflict"
        );
        let wt = f.0.join("other-checkout");
        assert!(git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                wt.to_str().unwrap(),
                "-b",
                "other"
            ]
        )
        .unwrap()
        .status
        .success());
        let other = prepare_root(wt.to_str().unwrap(), None).unwrap();
        assert_eq!(other.repo_key, ready.repo_key);
        assert_eq!(
            create(
                &c,
                "default",
                NewProject {
                    name: "Other".into(),
                    worktrees: None
                },
                &other
            )
            .unwrap_err()
            .code(),
            "project_root_conflict"
        );
        // The legacy team API defers Git inspection to G's first-use verification gate.
        let legacy = crate::teams::create_team(
            &c,
            &f.0,
            crate::teams::NewTeam { profile: None,
                name: "Legacy checkout".into(),
                kind: "main".into(),
                cwd: Some(wt.to_string_lossy().into()),
                worker_roles: None,
                members: None,
            },
        )
        .unwrap();
        let legacy_id = legacy.default_project_id.as_deref().unwrap();
        assert!(get(&c, &legacy.id, legacy_id).unwrap().repo_key.is_none());
        assert_eq!(
            verify(&c, &legacy.id, legacy_id, &other)
                .unwrap_err()
                .code(),
            "project_root_conflict"
        );
        assert!(c.execute("INSERT INTO projects(id,team_id,name,root_path,root_key,created_at,updated_at) VALUES('alias','default','alias',?1,?2,1,1)",params![p.root_path,p.root_key]).is_err());
        assert!(c.execute("INSERT INTO projects(id,team_id,name,root_path,root_key,repo_key,created_at,updated_at) VALUES('repo-alias','default','alias','different','different',?1,1,1)",[&ready.repo_key]).is_err());
        let nested_root = root.join("nested");
        std::fs::create_dir(&nested_root).unwrap();
        assert!(git(&nested_root, &["init", "--quiet"])
            .unwrap()
            .status
            .success());
        let nested_ready = prepare_root(nested_root.to_str().unwrap(), None).unwrap();
        assert_eq!(
            create(
                &c,
                "default",
                NewProject {
                    name: "Nested".into(),
                    worktrees: None
                },
                &nested_ready
            )
            .unwrap_err()
            .code(),
            "project_root_conflict"
        );
        #[cfg(windows)]
        assert_eq!(path_key(r"\\?\C:\Foo\BAR\"), path_key("c:/foo/bar"));
    }

    #[test]
    fn triggers_reject_service_ownership_foreign_links_and_snapshot_rewrite() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        c.execute_batch("INSERT INTO teams(id,name,kind,created_at) VALUES('foreign','Foreign','main',1),('service','Service','service',1)").unwrap();
        backfill(&c, &f.0).unwrap();
        assert!(c.execute_batch("INSERT INTO projects(id,team_id,name,created_at,updated_at) VALUES('s','service','S',1,1)").is_err());
        assert!(c
            .execute_batch(
                "UPDATE teams SET default_project_id='legacy-foreign' WHERE id='default'"
            )
            .is_err());
        assert!(c.execute_batch("INSERT INTO tasks(title,team_id,project_id,created_at,updated_at) VALUES('wrong','default','legacy-foreign',1,1)").is_err());
        c.execute_batch("INSERT INTO tasks(id,title,team_id,project_id,created_at,updated_at) VALUES(1,'t','default','legacy-default',1,1);
            INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,project_id,project_root,merge_branch,task_id,created_at,updated_at) VALUES(1,'timur','kaan','default','x','legacy-default','fixed-root','fixed-branch',1,1,1)").unwrap();
        assert!(c
            .execute_batch("UPDATE delegations SET project_id='legacy-foreign' WHERE id=1")
            .is_err());
        assert!(c
            .execute_batch("UPDATE delegations SET project_root='other' WHERE id=1")
            .is_err());
        assert!(c
            .execute_batch("UPDATE delegations SET merge_branch='other' WHERE id=1")
            .is_err());
        assert!(c.execute_batch("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id) VALUES('ayla','s','claude',1,'legacy-default',1)").is_err());
        c.execute_batch("INSERT INTO plans(id,team_id,leader,created_at,updated_at) VALUES(1,'foreign','l',1,1)").unwrap();
        assert!(c.execute_batch("INSERT INTO plan_items(plan_id,key,title,prompt,target,project_id,task_id) VALUES(1,'a','a','x','backend','legacy-foreign',1)").is_err());
        assert!(!c
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap());
    }

    #[test]
    fn reopened_backfill_preserves_explicit_cleared_selection() {
        let f = Fixture::new();
        let file = f.0.join("dex.db");
        let c = crate::db::open(&file).unwrap();
        set_default(&c, "default", None).unwrap();
        drop(c);
        let c = crate::db::open(&file).unwrap();
        assert!(crate::teams::get_team(&c, &f.0, "default")
            .unwrap()
            .default_project_id
            .is_none());
    }

    #[test]
    fn archived_projects_reject_new_persistent_assignments() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        archive(&c, "default", "legacy-default", |_| false).unwrap();
        assert!(c.execute_batch("INSERT INTO tasks(title,team_id,project_id,created_at,updated_at) VALUES('new','default','legacy-default',1,1)").is_err());
        assert!(c.execute_batch("INSERT INTO delegations(from_agent,to_agent,team_id,prompt,project_id,created_at,updated_at) VALUES('timur','kaan','default','new','legacy-default',1,1)").is_err());
    }

    #[test]
    fn core_creates_project_folder_under_team_workspace() {
        let f = Fixture::new();
        let state = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/project-core-1059").join(std::process::id().to_string());
        let core = crate::Core::start(crate::Paths {data_dir:state.join("data"),config_dir:state.join("config")},crate::Events::new(crate::events::NullSink)).unwrap();
        let workspace = f.0.join("category");
        std::fs::create_dir(&workspace).unwrap();
        let canonical = crate::teams::normalize_dir(workspace.to_str().unwrap()).unwrap();
        core.team_update("default",crate::teams::TeamPatch {workspace_root:Some(Some(canonical)),..Default::default()}).unwrap();
        // Stop discovery at this fixture's boundary, independent of the enclosing worktree.
        std::fs::write(workspace.join(".git"),"gitdir: missing-fixture-git-dir").unwrap();
        let p = core.project_create_folder("default",NewProject {name:"New App".into(),worktrees:None},None).unwrap();
        assert_eq!(p.root_path,Some(crate::teams::normalize_dir(workspace.join("NewApp").to_str().unwrap()).unwrap()));
        assert_eq!(p.name,"New App");
        assert_eq!(core.project_create_folder("default",NewProject {name:" /:*?".into(),worktrees:None},None).unwrap_err(),"invalid_folder_name");
        core.shutdown();
    }

    /// Closes a session without its exit starting queued work (as agents.rs's tests do).
    fn stop(core: &crate::Core, id: &str) {
        core.chain_guard.lock().unwrap().insert(id.into());
        core.ptys.kill(id);
        let t = std::time::Instant::now();
        while core.ptys.is_running(id) && t.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        core.chain_guard.lock().unwrap().remove(id);
    }

    /// #838 G2: the patron's project calls take the start gate. While a session bound to the
    /// project runs, archive is `project_busy`; an archive waits for a spawn holding the gate,
    /// and once archived the bound session's start fails closed (`project_unavailable`) without
    /// launching; reopen restores it. Codes are the same strings everywhere; a foreign team's id
    /// is `project_not_found`.
    #[test]
    fn core_project_calls_hold_the_start_gate() {
        use crate::agents::{StartOptions, CAPTURE};
        let tmp = std::env::temp_dir().join(format!("dex-g2-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let core = crate::Core::start(
            crate::Paths { data_dir: tmp.join("data"), config_dir: tmp.join("config") },
            crate::Events::new(crate::events::NullSink),
        )
        .unwrap();
        let f = Fixture::new();
        let root = f.dir("p1").to_string_lossy().into_owned();
        let new = || NewProject { name: "P1".into(), worktrees: None };
        let p1 = core.project_create("default", new(), &root, None).unwrap();
        assert_eq!(core.project_create("default", new(), &root, None).unwrap_err(), "project_root_conflict");
        assert_eq!(core.project_archive("foreign", &p1.id).unwrap_err(), "project_not_found");
        assert!(core.project_list("default", false).unwrap().iter().any(|p| p.id == p1.id));

        crate::db::with(&core.db, |c| {
            c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,project_id,project_root,merge_branch,created_at,updated_at)
                VALUES(1,'timur','kaan','default','iş','done',?1,?2,?3,1,1)", params![p1.id, root, p1.default_branch]).map_err(|e| e.to_string())?;
            c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id,cwd) VALUES('kaan','s1','claude',1,?1,1,?2)",
                params![p1.id, root]).map_err(|e| e.to_string())
        })
        .unwrap();
        let bound = SessionProject { project_id: Some(p1.id.clone()), delegation_id: Some(1) };
        assert_eq!(core.session_projects(Some("default")).unwrap().get("kaan"), Some(&bound));
        assert!(core.session_projects(Some("foreign")).unwrap().is_empty());

        // A running session bound to P1: busy; the record is unchanged.
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        core.agent_spawn("kaan", StartOptions::default()).unwrap();
        assert_eq!(CAPTURE.with(|c| c.borrow_mut().take().unwrap()).len(), 1);
        assert_eq!(core.project_archive("default", &p1.id).unwrap_err(), "project_busy");
        assert_eq!(core.project_update("default", &p1.id, ProjectPatch { worktrees: Some(false), ..Default::default() }, None, None).unwrap_err(), "project_busy");
        stop(&core, "kaan");

        // A spawn holding the gate: the archive waits for it.
        let gate = core.agent_start_gate.lock().unwrap();
        let (c2, id) = (core.clone(), p1.id.clone());
        let archive = std::thread::spawn(move || c2.project_archive("default", &id));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!archive.is_finished(), "archive ran past the start gate");
        drop(gate);
        assert!(archive.join().unwrap().unwrap().archived_at.is_some());

        // Archived: the bound start fails closed, nothing launched, the binding kept.
        CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
        assert_eq!(core.agent_spawn("kaan", StartOptions::default()).unwrap_err(), "project_unavailable");
        assert!(CAPTURE.with(|c| c.borrow_mut().take().unwrap()).is_empty() && !core.ptys.is_running("kaan"));
        assert_eq!(core.session_projects(None).unwrap().get("kaan"), Some(&bound));
        assert_eq!(core.project_set_default("default", Some(&p1.id)).unwrap_err(), "project_unavailable");
        assert_eq!(core.project_context("default", Some(&p1.id)).unwrap_err(), "project_unavailable");

        assert!(core.project_reopen("default", &p1.id).unwrap().archived_at.is_none());
        core.project_set_default("default", Some(&p1.id)).unwrap();
        let team = crate::db::with(&core.db, |c| crate::teams::get_team(c, &core.paths.data_dir, "default")).unwrap();
        assert_eq!((team.default_project_id.as_deref(), team.cwd.as_deref()), (Some(p1.id.as_str()), Some(root.as_str())));
        let ctx = core.project_context("default", None).unwrap();
        assert_eq!((ctx.project_id.as_deref(), ctx.project_root), (Some(p1.id.as_str()), Some(PathBuf::from(&root))));
        let p = core.project_verify("default", &p1.id).unwrap();
        assert_eq!((p.repo_key, p.default_branch), (p1.repo_key.clone(), p1.default_branch.clone()));
        assert_eq!(core.project_update("default", &p1.id, ProjectPatch::default(), None, Some("no-such-branch")).unwrap_err(), "project_branch_mismatch");
        assert_eq!(core.project_update("default", &p1.id, ProjectPatch { name: Some("App".into()), ..Default::default() }, None, None).unwrap().name, "App");

        core.shutdown();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
