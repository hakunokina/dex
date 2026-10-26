//! One DB-only context resolver. Job/session bindings outrank UI defaults; filesystem/Git
//! validation happens separately, outside the DB mutex, before a launch or merge.

use crate::projects::{self, PreparedRoot, Project, ProjectError};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMode {
    /// delegate/plan/GPU selection: multiple bound active projects require an explicit id.
    Selection,
    /// An administrative team workspace; a single bound project may still be automatic.
    Team,
    /// Resume the saved binding; pre-project sessions use the legacy team context.
    Session,
}
#[derive(Debug, Clone, Copy)]
pub struct ResolveRequest<'a> {
    /// Authoritative caller team, obtained from the DB/MCP identity.
    pub team_id: &'a str,
    pub agent_id: Option<&'a str>,
    /// Trusted launch/fix binding; external IPC must not supply this as authority.
    pub delegation_id: Option<i64>,
    pub project_id: Option<&'a str>,
    pub mode: ContextMode,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContext {
    pub team_id: String,
    pub agent_id: Option<String>,
    /// The requesting team's project; for a service worker this differs from team_id.
    pub project_team_id: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub project_root: Option<PathBuf>,
    pub repo_key: Option<String>,
    pub merge_branch: Option<String>,
    pub worktrees: bool,
    pub delegation_id: Option<i64>,
    pub cwd: PathBuf,
    pub result_path: Option<PathBuf>,
    pub workspace_root: PathBuf,
    pub suggested_workspace_root: Option<PathBuf>,
    pub team_memory_root: PathBuf,
    pub agent_memory_root: PathBuf,
    pub project_memory_root: Option<PathBuf>,
    /// Nominal slug path; E applies slug::memory_dir's existing id fallback at indexing time.
    pub private_memory_dir: Option<PathBuf>,
}

struct Job {
    team: String,
    service_team: Option<String>,
    worker: String,
    project: Option<String>,
    root: Option<String>,
    branch: Option<String>,
    report: Option<String>,
    worktree: Option<String>,
}
type Session = (Option<String>, Option<i64>, Option<String>);

/// DB-only resolution. Availability checks are explicit: validate after releasing the mutex.
pub fn resolve(
    conn: &Connection,
    data_dir: &Path,
    request: ResolveRequest<'_>,
) -> Result<ProjectContext, ProjectError> {
    let team = crate::teams::get_team(conn, data_dir, request.team_id)
        .map_err(|_| ProjectError::NotFound)?;
    let agent = if let Some(id) = request.agent_id {
        let a = conn
            .query_row(
                "SELECT team_id,kind,COALESCE(slug,id) FROM agents WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(ProjectError::NotFound)?;
        if a.0 != team.id {
            return Err(ProjectError::NotFound);
        }
        Some(a)
    } else {
        None
    };
    let saved: Option<Session> = if request.mode == ContextMode::Session {
        if let Some(id) = request.agent_id {
            conn.query_row(
                "SELECT project_id,delegation_id,cwd FROM agent_sessions WHERE agent_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
        } else {
            return Err(ProjectError::NotFound);
        }
    } else {
        None
    };
    if let Some((saved_project, saved_job, _)) = saved.as_ref() {
        if request.project_id.is_some() && request.project_id != saved_project.as_deref()
            || request.delegation_id.is_some() && request.delegation_id != *saved_job
        {
            return Err(ProjectError::Mismatch);
        }
    }
    let job_id = request
        .delegation_id
        .or_else(|| saved.as_ref().and_then(|s| s.1));
    let project_id = request
        .project_id
        .or_else(|| saved.as_ref().and_then(|s| s.0.as_deref()));
    let mut owner = team.id.clone();
    let mut snapshot_root = None;
    let mut snapshot_branch = None;
    let mut report = None;
    let mut job_cwd = None;
    let is_worker = agent.as_ref().is_some_and(|a| a.1 == "worker");
    let project = if let Some(id) = job_id {
        let job=conn.query_row("SELECT d.team_id,d.service_team_id,d.to_agent,d.project_id,d.project_root,d.merge_branch,d.result_path,
            (SELECT path FROM delegation_worktrees WHERE delegation_id=d.id AND removed_at IS NULL)
            FROM delegations d WHERE d.id=?1",[id],|r|Ok(Job{team:r.get(0)?,service_team:r.get(1)?,worker:r.get(2)?,project:r.get(3)?,root:r.get(4)?,branch:r.get(5)?,report:r.get(6)?,worktree:r.get(7)?})).optional()?.ok_or(ProjectError::NotFound)?;
        // A service worker sees only the job bound to that worker; caller paths confer no access.
        if job.team != team.id
            && !(job.service_team.as_deref() == Some(&team.id)
                && request.agent_id == Some(job.worker.as_str()))
            || is_worker && request.agent_id != Some(job.worker.as_str())
        {
            return Err(ProjectError::NotFound);
        }
        if project_id.is_some() && project_id != job.project.as_deref() {
            return Err(ProjectError::Mismatch);
        }
        owner = job.team;
        snapshot_root = job.root;
        snapshot_branch = job.branch;
        report = job.report;
        job_cwd = job.worktree;
        match job.project {
            Some(id) => Some(projects::get(conn, &owner, &id)?),
            None => {
                // Only genuinely folderless legacy administrative jobs may lack a binding.
                if conn.query_row(
                    "SELECT cwd IS NOT NULL FROM teams WHERE id=?1",
                    [&owner],
                    |r| r.get::<_, bool>(0),
                )? {
                    return Err(ProjectError::Unavailable);
                }
                None
            }
        }
    } else if saved.is_some() {
        match project_id {
            Some(id) => Some(projects::get(conn, &owner, id)?),
            None if saved.as_ref().is_some_and(|s| s.1.is_none() && s.2.is_none()) => {
                // Pre-project sessions have no binding or cwd. Keep their old repo context
                // without backfilling; multiple roots use the default mirrored by teams.cwd.
                let mut bound = projects::list(conn, &owner, false)?;
                bound.retain(|p| p.root_path.is_some());
                if bound.len() == 1 {
                    bound.pop()
                } else {
                    bound.into_iter().find(|p| {
                        team.default_project_id.as_deref() == Some(p.id.as_str())
                    })
                }
            }
            // An explicitly saved administrative cwd must remain a team session.
            None => None,
        }
    } else if is_worker {
        if project_id.is_some() {
            return Err(ProjectError::NotFound);
        }
        None
    } else {
        select(conn, &owner, project_id, request.mode)?
    };
    let base = crate::teams::service_dir(data_dir, &team.id);
    let workspace_root = team
        .workspace_root
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join("workspace"));
    let team_memory_root = team
        .team_memory_root
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join("memory"));
    let agent_memory_root = team
        .agent_memory_root
        .map(PathBuf::from)
        .unwrap_or_else(|| base.join("memory"));
    let (project_root, merge_branch, repo_key, worktrees) = match project.as_ref() {
        Some(p) => {
            if p.archived_at.is_some() {
                return Err(ProjectError::Unavailable);
            }
            let root = if job_id.is_some() {
                snapshot_root
            } else {
                p.root_path.clone()
            };
            if job_id.is_some() && p.root_path.is_some() && root.is_none() {
                return Err(ProjectError::Unavailable);
            }
            let branch = if job_id.is_some() {
                snapshot_branch
            } else {
                p.default_branch.clone()
            };
            (
                root.map(PathBuf::from),
                branch,
                p.repo_key.clone(),
                p.worktrees,
            )
        }
        None => (None, None, None, false),
    };
    let project_memory_root = project_root.as_ref().map(|p| p.join(".dex").join("memory"));
    // #2463: a worker's private memory belongs to its own team's project; a leader's, a service
    // job's and a project-less context's stay at the team anchor.
    let agent_memory_root = match project_memory_root.as_ref() {
        Some(root) if is_worker && project.as_ref().is_some_and(|p| p.team_id == team.id) => root.clone(),
        _ => agent_memory_root,
    };
    let private_memory_dir = agent
        .as_ref()
        .map(|a| agent_memory_root.join("agents").join(&a.2));
    let saved_cwd = saved.and_then(|s| s.2).map(PathBuf::from);
    let cwd = job_cwd
        .map(PathBuf::from)
        .or(saved_cwd)
        .or_else(|| {
            if team.kind == "service" {
                None
            } else {
                project_root.clone()
            }
        })
        .unwrap_or_else(|| workspace_root.clone());
    let result_path = report.map(PathBuf::from).or_else(|| {
        job_id.and_then(|id| {
            project_root.as_ref().map(|root| {
                if team.kind == "service" {
                    root.join("docs").join("research").join(format!(
                        "{id}-{}.md",
                        agent.as_ref().map_or("agent", |a| a.2.as_str())
                    ))
                } else {
                    root.join(".dex").join("results").join(format!("{id}.md"))
                }
            })
        })
    });
    Ok(ProjectContext {
        team_id: team.id,
        agent_id: request.agent_id.map(str::to_owned),
        project_team_id: project.as_ref().map(|p| p.team_id.clone()),
        project_id: project.as_ref().map(|p| p.id.clone()),
        project_name: project.as_ref().map(|p| p.name.clone()),
        project_memory_root,
        project_root,
        repo_key,
        merge_branch,
        worktrees,
        delegation_id: job_id,
        cwd,
        result_path,
        workspace_root,
        suggested_workspace_root: team.suggested_workspace_root.map(PathBuf::from),
        team_memory_root,
        agent_memory_root,
        private_memory_dir,
    })
}

fn select(
    conn: &Connection,
    team: &str,
    id: Option<&str>,
    mode: ContextMode,
) -> Result<Option<Project>, ProjectError> {
    if let Some(id) = id {
        return projects::get(conn, team, id).map(Some);
    }
    let active = projects::list(conn, team, false)?;
    let mut bound = active.iter().filter(|p| p.root_path.is_some());
    let first = bound.next();
    if bound.next().is_some() {
        return if mode == ContextMode::Team {
            Ok(None)
        } else {
            Err(ProjectError::Required)
        };
    }
    let selected = first
        .cloned()
        .or_else(|| active.into_iter().find(|p| p.root_path.is_none()));
    if selected.is_none()
        && mode == ContextMode::Selection
        && !projects::list(conn, team, true)?.is_empty()
    {
        return Err(ProjectError::Unavailable);
    }
    Ok(selected)
}

/// Filesystem and Git checks outside the DB mutex; no fallback to another project.
/// The caller applies projects::verify under the start gate, then fixes new job snapshots.
pub fn validate(context: &ProjectContext) -> Result<Option<PreparedRoot>, ProjectError> {
    let Some(root) = context.project_root.as_ref() else {
        return Ok(None);
    };
    let root = root.to_str().ok_or(ProjectError::Unavailable)?;
    let ready = projects::prepare_root(root, context.merge_branch.as_deref())?;
    if projects::path_key(root) != ready.root_key {
        return Err(ProjectError::Unavailable);
    }
    if context.repo_key.is_some() && context.repo_key != ready.repo_key {
        return Err(ProjectError::RootConflict);
    }
    Ok(Some(ready))
}
/// Merge requires the recorded target to be the primary checkout's current branch.
pub fn validate_merge(context: &ProjectContext) -> Result<PreparedRoot, ProjectError> {
    let ready = validate(context)?.ok_or(ProjectError::Unavailable)?;
    if context.merge_branch.is_none() || context.merge_branch != ready.head_branch {
        return Err(ProjectError::BranchMismatch);
    }
    Ok(ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{self, tests::Fixture, NewProject};

    #[test]
    fn selection_single_multi_missing_and_saved_job_are_stable() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let request = ResolveRequest {
            team_id: "default",
            agent_id: None,
            delegation_id: None,
            project_id: None,
            mode: ContextMode::Selection,
        };
        let admin = resolve(&c, &f.0, request).unwrap();
        assert!(admin.project_root.is_none());
        let root = f.dir("one");
        let ready = projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let one = projects::create(
            &c,
            "default",
            NewProject {
                name: "One".into(),
                worktrees: None,
            },
            &ready,
        )
        .unwrap();
        let selected = resolve(&c, &f.0, request).unwrap();
        assert_eq!(selected.project_id.as_deref(), Some(one.id.as_str()));
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,project_id,project_root,merge_branch,result_path,created_at,updated_at) VALUES(1,'timur','kaan','default','x',?1,?2,?3,'fixed-report',1,1)",rusqlite::params![one.id,one.root_path,one.default_branch]).unwrap();
        let root2 = f.dir("two");
        let ready2 = projects::prepare_root(root2.to_str().unwrap(), None).unwrap();
        let two = projects::create(
            &c,
            "default",
            NewProject {
                name: "Two".into(),
                worktrees: None,
            },
            &ready2,
        )
        .unwrap();
        projects::set_default(&c, "default", Some(&two.id)).unwrap();
        assert_eq!(
            resolve(&c, &f.0, request).unwrap_err().code(),
            "project_required"
        );
        let job = resolve(
            &c,
            &f.0,
            ResolveRequest {
                agent_id: Some("kaan"),
                delegation_id: Some(1),
                ..request
            },
        )
        .unwrap();
        assert_eq!(job.project_id.as_deref(), Some(one.id.as_str()));
        assert_eq!(
            job.result_path.as_deref(),
            Some(std::path::Path::new("fixed-report"))
        );
        assert_eq!(
            resolve(
                &c,
                &f.0,
                ResolveRequest {
                    agent_id: Some("kaan"),
                    delegation_id: Some(1),
                    project_id: Some(&two.id),
                    ..request
                }
            )
            .unwrap_err()
            .code(),
            "project_mismatch"
        );
        assert_eq!(
            resolve(
                &c,
                &f.0,
                ResolveRequest {
                    project_id: Some("foreign"),
                    ..request
                }
            )
            .unwrap_err()
            .code(),
            "project_not_found"
        );
        std::fs::rename(&root2, f.0.join("moved-two")).unwrap();
        assert_eq!(
            resolve(&c, &f.0, request).unwrap_err().code(),
            "project_required",
            "unavailable roots do not reduce ambiguity"
        );
        assert_eq!(
            validate(
                &resolve(
                    &c,
                    &f.0,
                    ResolveRequest {
                        project_id: Some(&two.id),
                        ..request
                    }
                )
                .unwrap()
            )
            .unwrap_err()
            .code(),
            "project_unavailable"
        );
        assert!(resolve(
            &c,
            &f.0,
            ResolveRequest {
                mode: ContextMode::Team,
                ..request
            }
        )
        .unwrap()
        .project_id
        .is_none());
    }

    #[test]
    fn service_session_and_chain_use_requester_project_and_own_memory() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let root = f.dir("main");
        let ready = projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let p = projects::create(
            &c,
            "default",
            NewProject {
                name: "Main".into(),
                worktrees: None,
            },
            &ready,
        )
        .unwrap();
        let service = crate::teams::create_team(
            &c,
            &f.0,
            crate::teams::NewTeam { profile: None,
                name: "Research".into(),
                kind: "service".into(),
                cwd: None,
                worker_roles: None,
                members: None,
            },
        )
        .unwrap();
        let worker = crate::teams::list_agents(&c)
            .unwrap()
            .into_iter()
            .find(|a| a.team_id == service.id && a.kind == "worker")
            .unwrap();
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,service_team_id,prompt,project_id,project_root,merge_branch,created_at,updated_at) VALUES(1,'timur',?1,'default',?2,'x',?3,?4,?5,1,1)",rusqlite::params![worker.id,service.id,p.id,p.root_path,p.default_branch]).unwrap();
        let req = ResolveRequest {
            team_id: &service.id,
            agent_id: Some(&worker.id),
            delegation_id: Some(1),
            project_id: None,
            mode: ContextMode::Selection,
        };
        let ctx = resolve(&c, &f.0, req).unwrap();
        assert_eq!(ctx.project_team_id.as_deref(), Some("default"));
        assert_eq!(ctx.project_id.as_deref(), Some(p.id.as_str()));
        assert_eq!(
            ctx.cwd,
            crate::teams::service_dir(&f.0, &service.id).join("workspace")
        );
        assert_eq!(
            ctx.team_memory_root,
            crate::teams::service_dir(&f.0, &service.id).join("memory")
        );
        assert!(ctx.result_path.as_ref().unwrap().ends_with(format!(
            "docs/research/1-{}.md",
            crate::slug::agent(&c, &worker.id).unwrap()
        )));
        c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,project_id,delegation_id,cwd) VALUES(?1,'s','claude',1,?2,1,?3)",rusqlite::params![worker.id,p.id,ctx.cwd.to_string_lossy()]).unwrap();
        let resumed = resolve(
            &c,
            &f.0,
            ResolveRequest {
                delegation_id: None,
                mode: ContextMode::Session,
                ..req
            },
        )
        .unwrap();
        assert_eq!(resumed.project_id, ctx.project_id);
        assert_eq!(resumed.cwd, ctx.cwd);
        assert_eq!(
            resolve(
                &c,
                &f.0,
                ResolveRequest {
                    delegation_id: None,
                    project_id: Some(&p.id),
                    ..req
                }
            )
            .unwrap_err()
            .code(),
            "project_not_found",
            "a worker cannot self-select a project"
        );
        assert_eq!(
            resolve(
                &c,
                &f.0,
                ResolveRequest {
                    team_id: "default",
                    agent_id: Some("ayla"),
                    ..req
                }
            )
            .unwrap_err()
            .code(),
            "project_not_found"
        );
        assert_eq!(projects::list(&c, &service.id, true).unwrap().len(), 0);
    }

    #[test]
    fn merge_validation_does_not_checkout_the_recorded_branch() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let root = f.dir("repo");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success());
        };
        std::fs::write(root.join("base.txt"), "base").unwrap();
        run(&["add", "base.txt"]);
        run(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ]);
        let ready = projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let p = projects::create(
            &c,
            "default",
            NewProject {
                name: "App".into(),
                worktrees: None,
            },
            &ready,
        )
        .unwrap();
        let ctx = resolve(
            &c,
            &f.0,
            ResolveRequest {
                team_id: "default",
                agent_id: None,
                delegation_id: None,
                project_id: Some(&p.id),
                mode: ContextMode::Selection,
            },
        )
        .unwrap();
        assert!(validate_merge(&ctx).is_ok());
        run(&["switch", "--quiet", "-c", "other"]);
        assert_eq!(
            validate_merge(&ctx).unwrap_err().code(),
            "project_branch_mismatch"
        );
        assert_eq!(
            projects::prepare_root(root.to_str().unwrap(), Some("nonexistent"))
                .unwrap_err()
                .code(),
            "project_branch_mismatch"
        );
        run(&["checkout", "--quiet", "--detach"]);
        assert_eq!(
            validate_merge(&ctx).unwrap_err().code(),
            "project_branch_mismatch"
        );
    }

    #[test]
    fn archived_bound_project_does_not_turn_into_an_unbound_repo_job() {
        let f = Fixture::new();
        let c = crate::db::open(&f.0.join("dex.db")).unwrap();
        let root = f.dir("bound");
        let t = crate::teams::create_team(
            &c,
            &f.0,
            crate::teams::NewTeam { profile: None,
                name: "Bound".into(),
                kind: "main".into(),
                cwd: Some(root.to_string_lossy().into()),
                worker_roles: None,
                members: None,
            },
        )
        .unwrap();
        projects::archive(&c, &t.id, t.default_project_id.as_deref().unwrap(), |_| {
            false
        })
        .unwrap();
        let req = ResolveRequest {
            team_id: &t.id,
            agent_id: None,
            delegation_id: None,
            project_id: None,
            mode: ContextMode::Selection,
        };
        assert_eq!(
            resolve(&c, &f.0, req).unwrap_err().code(),
            "project_unavailable"
        );
        assert!(resolve(
            &c,
            &f.0,
            ResolveRequest {
                mode: ContextMode::Team,
                ..req
            }
        )
        .unwrap()
        .project_root
        .is_none());
    }
}
