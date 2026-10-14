//! The Tauri commands of `src-tauri`, one to one: same names, same camelCase argument names,
//! same results. PTY and connection methods (they need the connection) are in `server`.

use dex_core::projects::ProjectError;
use dex_core::Core;
use serde::Serialize;
use serde_json::Value;

use crate::protocol::{arg, Error};

fn ok<T: Serialize>(r: Result<T, String>) -> Result<Value, Error> {
    let v = r.map_err(Error::failed)?;
    serde_json::to_value(v).map_err(|e| Error::failed(e.to_string()))
}

/// #838 G2: a project-layer call keeps its stable code — `{code, message}`, the message the same
/// text the in-app command rejects with (the client sees one answer on either transport).
fn project<T: Serialize>(r: Result<T, String>) -> Result<Value, Error> {
    let v = r.map_err(|e| match ProjectError::from_code(&e) {
        Some(p) => Error::new(p.code(), e),
        None => Error::failed(e),
    })?;
    serde_json::to_value(v).map_err(|e| Error::failed(e.to_string()))
}

type Handler = fn(&Core, &Value) -> Result<Value, Error>;

/// Blocking (DB, file and — for providers/codex — network calls): run it off the async runtime.
pub fn call(core: &Core, method: &str, p: &Value) -> Result<Value, Error> {
    let h = handler(method)
        .ok_or_else(|| Error::new("unknown_method", format!("bilinmeyen metot: {method}")))?;
    h(core, p)
}

/// One table → the dispatcher and `METHODS` (advertised in `auth.hello`, so a client can tell
/// what an older dexd lacks without trying it).
macro_rules! handlers {
    ($($name:literal => $h:expr,)*) => {
        pub const METHODS: &[&str] = &[$($name),*];

        fn handler(method: &str) -> Option<Handler> {
            let h: Handler = match method {
                $($name => $h,)*
                _ => return None,
            };
            Some(h)
        }
    };
}

handlers! {
        "inbox_submit" => |core,p| {
            let input = serde_json::from_value(p.clone()).map_err(|e| Error::bad_request(format!("inbox_submit: {e}")))?;
            let reply = core.inbox_submit(input).map_err(|e| Error::new(e.code, e.message))?;
            serde_json::to_value(reply).map_err(|e| Error::failed(e.to_string()))
        },
        "agent_context" => |core,p| ok(core.agent_context(&arg::<String>(p,"agentId")?)),
        "team_context" => |core,p| ok(core.team_context(&arg::<String>(p,"teamId")?)),
        "usage_summary" => |core,p| ok(core.usage_summary(&arg::<String>(p,"teamId")?,arg(p,"since")?)),
        "usage_delegation" => |core,p| ok(core.usage_delegation(arg(p,"id")?)),
        "settings_usage_prices" => |core,_| ok(core.settings_usage_prices()),
        "settings_set_usage_prices" => |core,p| ok(core.settings_set_usage_prices(arg(p,"prices")?)),
        "core_status" => |core, _| ok(core.core_status()),
        "pty_kill" => |core, p| {
            dex_core::diagnostics::event("pty_kill", serde_json::json!({"agent": arg::<String>(p, "id")?, "reason": "user"}));
            core.agent_stop(&arg::<String>(p, "id")?);
            Ok(Value::Null)
        },
        "pty_write" => |core, p| {
            ok(core.ptys.write(
                &arg::<String>(p, "id")?,
                arg::<String>(p, "data")?.as_bytes(),
            ))
        },
        "task_list" => |core, p| ok(core.task_list(arg::<Option<String>>(p, "teamId")?.as_deref())),
        "task_create" => |core, p| ok(core.task_create(arg(p, "task")?)),
        "task_update" => |core, p| ok(core.task_update(arg(p, "id")?, arg(p, "patch")?)),
        "task_delete" => |core, p| ok(core.task_delete(arg(p, "id")?)),
        "layout_load" => |core, _| ok(core.layout_load()),
        "layout_save" => |core, p| ok(core.layout_save(&arg::<Value>(p, "layout")?)),
        "team_list" => |core, _| ok(core.team_list()),
        "team_create" => |core, p| project(core.team_create(arg(p, "team")?)),
        "team_folder_preview" => |core, p| project(core.team_folder_preview(&arg::<String>(p, "name")?)),
        "team_clone_template" => |core, p| ok(core.team_clone_template(&arg::<String>(p,"teamId")?)),
        "team_update" => |core, p| ok(core.team_update(&arg::<String>(p, "id")?, arg(p, "patch")?)),
        "team_delete" => |core, p| {
            ok(core.team_delete(&arg::<String>(p, "id")?, arg::<Option<bool>>(p, "stopAgents")?.unwrap_or(false)))
        },
        "team_delete_info" => |core, p| ok(core.team_delete_info(&arg::<String>(p, "id")?)),
        "agent_list" => |core, _| ok(core.agent_list()),
        "agent_create" => |core, p| ok(core.agent_create(arg(p, "agent")?)),
        "agent_update" => {
            |core, p| ok(core.agent_update(&arg::<String>(p, "id")?, arg(p, "patch")?))
        },
        "agent_delete" => |core, p| ok(core.agent_delete(&arg::<String>(p, "id")?)),
        "agent_start" => |core, p| {
            ok(core.agent_start(
                &arg::<String>(p, "id")?,
                arg::<Option<dex_core::agents::StartOptions>>(p, "options")?.unwrap_or_default(),
            ))
        },
        "agent_status_list" => |core, _| ok(Ok(core.agent_status_list())),
        "agent_input" => |core, p| {
            core.agent_input(&arg::<String>(p, "id")?, arg(p, "draft")?);
            Ok(Value::Null)
        },
        "leader_reset" => |core, p| {
            ok(core.leader_reset(&arg::<String>(p, "id")?, &arg::<String>(p, "action")?))
        },
        "agent_first_reply_retry" => {
            |core, p| ok(core.agent_first_reply_retry(&arg::<String>(p, "id")?))
        },
        "agent_first_reply_cancel" => {
            |core, p| ok(core.agent_first_reply_cancel(&arg::<String>(p, "id")?))
        },
        "first_reply_get" => |core, _| ok(core.first_reply_get()),
        "first_reply_set" => {
            |core, p| ok(core.first_reply_set(&arg::<String>(p, "provider")?, arg(p, "secs")?))
        },
        "chain_get" => |core, _| ok(core.chain_get()),
        "chain_set_order" => |core, p| ok(core.chain_set_order(arg(p, "order")?)),
        "chain_set_disabled" => |core, p| ok(core.chain_set_disabled(arg(p, "ids")?)),
        "chain_set_agent" => {
            |core, p| ok(core.chain_set_agent(&arg::<String>(p, "id")?, arg(p, "off")?))
        },
        "chain_back" => |core, p| ok(core.chain_back(&arg::<String>(p, "id")?)),
        "codex_models" => |core, _| ok(core.codex_models()),
        "model_news" => |core, _| ok(core.model_news()),
        "model_news_dismiss" => |core, p| ok(core.model_news_dismiss(arg(p, "keys")?)),
        "team_set_workers_model" => |core, p| {
            ok(core.team_set_workers_model(&arg::<String>(p, "teamId")?, arg(p, "target")?))
        },
        "agent_apply_pending" => |core, p| ok(core.agent_apply_pending(&arg::<String>(p, "id")?)),
        "chat_history" => |core, p| {
            ok(core.chat_history(
                &arg::<String>(p, "agentId")?,
                arg::<Option<u64>>(p, "before")?,
                arg::<Option<u32>>(p, "limit")?,
            ))
        },
        "chat_send" => {
            |core, p| ok(core.chat_send(&arg::<String>(p, "agentId")?, &arg::<String>(p, "text")?))
        },
        "chat_screen" => |core, p| ok(core.chat_screen(&arg::<String>(p, "agentId")?)),
        "memory_graph" => |core, p| ok(core.memory_graph(&arg::<String>(p, "teamId")?)),
        "memory_read" => |core, p| ok(core.memory_read(&arg::<String>(p, "teamId")?, &arg::<String>(p, "id")?)),
        "project_memory_graph" => |core, p| {
            project(core.project_memory_graph(&arg::<String>(p, "teamId")?, &arg::<String>(p, "projectId")?))
        },
        "project_memory_read" => |core, p| {
            project(core.project_memory_read(
                &arg::<String>(p, "teamId")?,
                &arg::<String>(p, "projectId")?,
                &arg::<String>(p, "id")?,
            ))
        },
        "team_memory_graph" => |core, p| project(core.team_memory_graph(&arg::<String>(p, "teamId")?)),
        "team_memory_read" => |core, p| {
            project(core.team_memory_read(&arg::<String>(p, "teamId")?, &arg::<String>(p, "id")?))
        },
        "project_list" => |core, p| {
            project(core.project_list(&arg::<String>(p, "teamId")?, arg::<Option<bool>>(p, "includeArchived")?.unwrap_or(false)))
        },
        "project_create" => |core, p| {
            if arg::<Option<bool>>(p,"createFolder")?.unwrap_or(false) {
                if arg::<Option<String>>(p,"root")?.is_some() { return Err(Error::bad_request("createFolder ile root birlikte verilemez")); }
                return project(core.project_create_folder(&arg::<String>(p,"teamId")?,arg(p,"project")?,arg::<Option<String>>(p,"branch")?.as_deref()));
            }
            project(core.project_create(
                &arg::<String>(p, "teamId")?,
                arg(p, "project")?,
                &arg::<String>(p, "root")?,
                arg::<Option<String>>(p, "branch")?.as_deref(),
            ))
        },
        "project_update" => |core, p| {
            project(core.project_update(
                &arg::<String>(p, "teamId")?,
                &arg::<String>(p, "id")?,
                arg::<Option<dex_core::projects::ProjectPatch>>(p, "patch")?.unwrap_or_default(),
                arg::<Option<String>>(p, "root")?.as_deref(),
                arg::<Option<String>>(p, "branch")?.as_deref(),
            ))
        },
        "project_archive" => |core, p| project(core.project_archive(&arg::<String>(p, "teamId")?, &arg::<String>(p, "id")?)),
        "project_reopen" => |core, p| project(core.project_reopen(&arg::<String>(p, "teamId")?, &arg::<String>(p, "id")?)),
        "project_set_default" => |core, p| {
            project(core.project_set_default(&arg::<String>(p, "teamId")?, arg::<Option<String>>(p, "id")?.as_deref()))
        },
        "project_context" => |core, p| {
            project(core.project_context(&arg::<String>(p, "teamId")?, arg::<Option<String>>(p, "projectId")?.as_deref()))
        },
        "project_verify" => |core, p| project(core.project_verify(&arg::<String>(p, "teamId")?, &arg::<String>(p, "id")?)),
        "session_projects" => |core, p| project(core.session_projects(arg::<Option<String>>(p, "teamId")?.as_deref())),
        "role_list" => |core, _| ok(core.role_list()),
        "delegation_list" => |core, _| ok(core.delegation_list()),
        "delegation_ack" => {
            |core, p| ok(core.delegation_ack(arg(p, "id")?, arg(p, "error")?, arg(p, "queued")?))
        },
        "delegation_session_exited" => {
            |core, p| ok(core.delegation_session_exited(&arg::<String>(p, "agentId")?))
        },
        "delegation_seen" => |core, p| ok(core.delegation_seen(&arg::<String>(p, "agentId")?)),
        "question_list" => |core, _| ok(core.question_list()),
        "question_answer" => {
            |core, p| ok(core.question_answer(arg(p, "id")?, &arg::<String>(p, "text")?))
        },
        "delegation_stalled" => {
            |core, p| ok(core.delegation_stalled(&arg::<String>(p, "agentId")?, arg(p, "minutes")?))
        },
        "provider_list" => |core, _| ok(core.provider_list()),
        "gateway_auth_start" => |core,p| ok(core.gateway_auth_start(arg::<Option<String>>(p,"accountId")?.as_deref())),
        "gateway_auth_status" => |core,_| ok(core.gateway_auth_status()),
        "gateway_auth_logout" => |core,p| ok(core.gateway_auth_logout(arg::<Option<String>>(p,"accountId")?.as_deref())),
        "gateway_models" => |core,_| ok(core.gateway_models()),
        "provider_import_routes" => |core, _| ok(core.provider_import_routes()),
        "provider_set_token" => |core, p| {
            ok(core.provider_set_token(&arg::<String>(p, "id")?, &arg::<String>(p, "token")?))
        },
        "provider_local_set_base_url" => {
            |core, p| ok(core.provider_local_set_base_url(&arg::<String>(p, "url")?))
        },
        "provider_local_status" => |core, _| ok(core.provider_local_status()),
        "provider_local_add" => |core, p| ok(core.provider_local_add(&arg::<String>(p, "name")?)),
        "provider_local_delete" => {
            |core, p| ok(core.provider_local_delete(&arg::<String>(p, "name")?))
        },
        "plan_resume" => |core, _| ok(core.plan_resume()),
        "plan_list" => |core, _| ok(core.plan_list()),
        "proposal_list" => |core, _| ok(core.proposal_list()),
        "proposal_decide" => |core, p| {
            ok(core.proposal_decide(
                arg(p, "id")?,
                arg(p, "approve")?,
                arg::<Option<String>>(p, "name")?.as_deref(),
            ))
        },
        "gpu_config_get" => |core, _| ok(core.gpu_config_get()),
        "gpu_limits_get" => |core, _| ok(core.gpu_limits_get()),
        "gpu_limits_set" => |core, p| {
            // Ayrıştırma hataları kullanıcı verisini (yönetici şifresini) geri yansıtmaz.
            let token = p.get("adminToken").and_then(Value::as_str).ok_or_else(|| Error::bad_request("yönetici şifresi gerekli"))?;
            let limits = serde_json::from_value(p.get("limits").cloned().unwrap_or(Value::Null))
                .map_err(|_| Error::bad_request("geçersiz GPU limitleri"))?;
            let reason = match p.get("reason") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.as_str()),
                _ => return Err(Error::bad_request("geçersiz sebep")),
            };
            ok(core.gpu_limits_set(token, limits, reason))
        },
        "gpu_config_set" => |core, p| ok(core.gpu_config_set(arg(p, "config")?)),
        "gpu_overview" => |core, p| ok(core.gpu_overview(arg::<Option<bool>>(p, "refresh")?.unwrap_or(false))),
        "gpu_request_list" => |core, _| ok(core.gpu_request_list()),
        "gpu_request_decide" => |core, p| {
            ok(core.gpu_request_decide(arg(p, "id")?, arg(p, "approve")?, arg::<Option<i64>>(p, "budget")?))
        },
        "agent_processes" => |core, p| {
            ok(dex_core::proc::agent_processes(core, arg::<Option<String>>(p, "agent")?.as_deref()))
        },
        "kill_agent_process" => |core, p| {
            ok(dex_core::proc::kill_agent_process(core, &arg::<String>(p, "agent")?, arg(p, "pid")?))
        },
        "limits_get" => |core, _| ok(core.limits_get()),
        "limits_set" => |core, p| ok(core.limits_set(arg(p, "limits")?)),
        "note_get" => |core, p| ok(core.note_get(&arg::<String>(p, "scope")?)),
        "note_set" => {
            |core, p| ok(core.note_set(&arg::<String>(p, "scope")?, &arg::<String>(p, "body")?))
        },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::LOCAL_METHODS;
    use dex_core::events::NullSink;
    use dex_core::{Events, Paths};

    #[test]
    fn inbox_host_rpc_is_advertised_and_has_stable_validation_errors() {
        let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../.dex/tmp/inbox-rpc-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let core=Core::start_bare(Paths{data_dir:root.join("data"),config_dir:root.join("config")},Events::new(NullSink)).unwrap();
        assert!(METHODS.contains(&"inbox_submit"));
        for (params,code) in [(serde_json::json!({}),"bad_request"),(serde_json::json!({"teamId":"default"}),"inbox_empty"),(serde_json::json!({"teamId":"default","text":"x"}),"inbox_no_assistant"),(serde_json::json!({"teamId":"default","text":"x","path":"evil"}),"bad_request")] {
            assert_eq!(call(&core,"inbox_submit",&params).unwrap_err().code,code);
        }
        core.shutdown();
    }

    #[test]
    fn gateway_auth_rpcs_are_advertised_and_remote_hosts_refuse_local_login() {
        let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../.dex/tmp/1072/rpc-{}",std::process::id()));
        let core=Core::start_bare(Paths {data_dir:root.join("data"),config_dir:root.join("config")},Events::new(NullSink)).unwrap();
        for method in ["gateway_auth_start","gateway_auth_status","gateway_auth_logout","gateway_models"] {
            assert!(METHODS.contains(&method));
            let error=call(&core,method,&serde_json::json!({})).unwrap_err();
            assert_eq!(error.message,"giriş yalnız yerel Dex'ten");
        }
        assert_eq!(call(&core,"gateway_auth_start",&serde_json::json!({"accountId":12})).unwrap_err().code,"bad_request");
        core.shutdown();
    }

    #[test]
    fn usage_rpc_ingests_and_prices_a_delegation() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../.dex/tmp/usage-rpc-{}",std::process::id()));
        let core = Core::start(Paths { data_dir:root.join("data"),config_dir:root.join("config") },Events::new(NullSink)).unwrap();
        let transcript=root.join("session.jsonl");
        std::fs::write(&transcript,concat!(r#"{"type":"assistant","timestamp":"2026-10-05T10:00:00Z","message":{"id":"m1","model":"fixture-model","usage":{"input_tokens":100,"output_tokens":50}}}"#,"\n")).unwrap();
        dex_core::db::with(&core.db,|c| {
            c.execute("INSERT INTO cost_sources(id,engine,session,path) VALUES(1,'claude','fixture-session',?1)",[transcript.to_string_lossy().as_ref()]).map_err(|e|e.to_string())?;
            c.execute("INSERT INTO cost_sessions(source_id,agent_id,team_id,delegation_id,provider,model,started_at) VALUES(1,'kaan','default',42,'glm','alias',0)",[]).map_err(|e|e.to_string())?;
            Ok(())
        }).unwrap();
        let out=call(&core,"usage_summary",&serde_json::json!({"teamId":"default"})).unwrap();
        assert_eq!(out["total"]["tokens"]["input"],100);
        let context = call(&core,"agent_context",&serde_json::json!({"agentId":"kaan"})).unwrap();
        assert_eq!(context["tokens"],100);
        assert_eq!(context["sessionId"],"fixture-session");
        assert_eq!(context["window"],Value::Null);
        let team = call(&core,"team_context",&serde_json::json!({"teamId":"default"})).unwrap();
        assert_eq!(team["kaan"],context);
        assert_eq!(call(&core,"team_context",&serde_json::json!({"teamId":"other"})).unwrap(),serde_json::json!({}));
        assert!(call(&core,"agent_context",&serde_json::json!({"agentId":"missing"})).is_err());
        assert!(METHODS.contains(&"agent_context") && METHODS.contains(&"team_context"));
        assert_eq!(out["total"]["usd"],Value::Null);
        assert_eq!(out["warnings"],serde_json::json!([]));
        call(&core,"settings_set_usage_prices",&serde_json::json!({"prices":[{"provider":"glm","model":"fixture-model",
            "rates":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}]})).unwrap();
        let out=call(&core,"usage_delegation",&serde_json::json!({"id":42})).unwrap();
        assert_eq!(out["total"]["tokens"]["input"],100);
        assert_eq!(out["total"]["usd"],0.0002);
        assert_eq!(out["agents"]["kaan"]["usd"],out["delegations"]["42"]["usd"]);
        assert!(call(&core,"settings_usage_prices",&serde_json::json!({})).unwrap().as_array().unwrap().iter().any(|p|p["model"]=="fixture-model"));
        assert_eq!(call(&core,"usage_summary",&serde_json::json!({"teamId":"other"})).unwrap()["total"]["tokens"]["input"],0);
        assert!(call(&core,"usage_summary",&serde_json::json!({"teamId":"default","since":-1})).is_err());
        assert!(METHODS.contains(&"usage_summary") && METHODS.contains(&"usage_delegation"));
        core.shutdown();
    }

    /// #838 G2: a project call answers the same over RPC as the in-app command (both call the
    /// Core): the same value, and an error's code with the same text the command rejects with.
    /// Every project method is advertised (`auth.hello` methods).
    #[test]
    fn project_calls_answer_the_same_on_rpc_and_in_app() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../.dex/tmp/project-rpc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let core = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(NullSink)).unwrap();
        let repo = |name: &str| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            assert!(std::process::Command::new("git").arg("-C").arg(&dir).args(["init", "--quiet", "--initial-branch=main"]).status().unwrap().success());
            dex_core::teams::normalize_dir(dir.to_str().unwrap()).unwrap()
        };
        let (one, two) = (repo("one"), repo("two"));
        let rpc = |m: &str, p: Value| call(&core, m, &p);
        let same = |r: Result<Value, Error>, app: Result<Value, String>| match (r, app) {
            // Two writes in a row: only their clock may differ.
            (Ok(mut r), Ok(mut app)) => {
                for v in [&mut r, &mut app] {
                    v.as_object_mut().map(|o| o.remove("updatedAt"));
                }
                assert_eq!(r, app)
            }
            (Err(r), Err(app)) => {
                let code = ProjectError::from_code(&app).map_or("failed", ProjectError::code);
                assert_eq!((r.code, r.message.as_str()), (code, app.as_str()));
            }
            (r, app) => panic!("RPC {:?} ≠ uygulama {app:?}", r.map_err(|e| e.code)),
        };
        fn json<T: Serialize>(v: T) -> Value {
            serde_json::to_value(v).unwrap()
        }

        let created = rpc("project_create", serde_json::json!({"teamId": "default", "project": {"name": "One"}, "root": one})).unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!((created["rootPath"].as_str(), created["defaultBranch"].as_str()), (Some(one.as_str()), Some("main")));
        let app = core.project_create("default", dex_core::projects::NewProject { name: "Two".into(), worktrees: None }, &two, None).map(json);
        let two_id = app.as_ref().unwrap()["id"].as_str().unwrap().to_string();
        same(rpc("project_list", serde_json::json!({"teamId": "default"})), core.project_list("default", false).map(json));
        same(
            rpc("project_create", serde_json::json!({"teamId": "default", "project": {"name": "Again"}, "root": one})),
            core.project_create("default", dex_core::projects::NewProject { name: "Again".into(), worktrees: None }, &one, None).map(json),
        );
        for m in ["project_archive", "project_reopen", "project_verify"] {
            let app = match m {
                "project_archive" => core.project_archive("foreign", &id),
                "project_reopen" => core.project_reopen("foreign", &id),
                _ => core.project_verify("foreign", &id),
            };
            same(rpc(m, serde_json::json!({"teamId": "foreign", "id": id})), app.map(json));
        }
        assert_eq!(rpc("project_archive", serde_json::json!({"teamId": "foreign", "id": id})).unwrap_err().code, "project_not_found");
        same(
            rpc("project_update", serde_json::json!({"teamId": "default", "id": id, "branch": "nope"})),
            core.project_update("default", &id, Default::default(), None, Some("nope")).map(json),
        );
        same(
            rpc("project_update", serde_json::json!({"teamId": "default", "id": id, "patch": {"name": "Uno"}})),
            core.project_update("default", &id, dex_core::projects::ProjectPatch { name: Some("Uno".into()), worktrees: None }, None, None).map(json),
        );
        // Two bound projects: the team context has none selected until one is chosen.
        same(rpc("project_context", serde_json::json!({"teamId": "default"})), core.project_context("default", None).map(json));
        assert_eq!(rpc("project_context", serde_json::json!({"teamId": "default"})).unwrap()["projectId"], Value::Null);
        same(rpc("project_set_default", serde_json::json!({"teamId": "default", "id": two_id})), core.project_set_default("default", Some(&two_id)).map(json));
        same(rpc("project_context", serde_json::json!({"teamId": "default", "projectId": id})), core.project_context("default", Some(&id)).map(json));
        same(rpc("session_projects", serde_json::json!({})), core.session_projects(None).map(json));
        // Memory: one project's scopes, or the team's alone; an unknown project is not found
        // (an unknown team keeps the memory API's own error, `failed`).
        same(rpc("project_memory_graph", serde_json::json!({"teamId": "default", "projectId": id})), core.project_memory_graph("default", &id).map(json));
        same(rpc("team_memory_graph", serde_json::json!({"teamId": "default"})), core.team_memory_graph("default").map(json));
        same(
            rpc("project_memory_graph", serde_json::json!({"teamId": "foreign", "projectId": id})),
            core.project_memory_graph("foreign", &id).map(json),
        );
        same(
            rpc("project_memory_read", serde_json::json!({"teamId": "default", "projectId": "nope", "id": "project/nope/shared/x"})),
            core.project_memory_read("default", "nope", "project/nope/shared/x").map(json),
        );
        assert_eq!(rpc("project_memory_graph", serde_json::json!({"teamId": "default", "projectId": "nope"})).unwrap_err().code, "project_not_found");
        assert!(rpc("project_archive", serde_json::json!({"teamId": "default", "id": id})).unwrap()["archivedAt"].is_i64());
        // An older-shape call keeps its envelope; a missing argument is a bad request.
        assert_eq!(rpc("project_list", serde_json::json!({})).unwrap_err().code, "bad_request");
        for m in [
            "project_list", "project_create", "project_update", "project_archive", "project_reopen", "project_set_default",
            "project_context", "project_verify", "session_projects", "project_memory_graph", "project_memory_read",
            "team_memory_graph", "team_memory_read",
        ] {
            assert!(METHODS.contains(&m), "{m}");
        }
        core.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The shell's `generate_handler!` list that contains `marker`, as bare command names.
    fn tauri_commands(marker: &str) -> Vec<String> {
        let shell = include_str!("../../../src-tauri/src/lib.rs");
        let list = shell
            .split("generate_handler![")
            .skip(1)
            .map(|rest| &rest[..rest.find(']').unwrap()])
            .find(|l| l.contains(marker))
            .unwrap_or_else(|| panic!("no handler list with {marker}"));
        list.split([',', '\n'])
            .filter_map(|s| s.trim().rsplit_once("::").map(|(_, n)| n.to_string()))
            .collect()
    }

    /// #150 (B): the shell answers only what is this machine's screen, keyring and window; every
    /// core call goes to dexd. Turned around from A0: a new Tauri command must be CLIENT_ONLY —
    /// add it there (and to src/api/clientOnly.ts) or, better, to dexd.
    const CLIENT_ONLY: [&str; 17] = [
        // The local core's endpoint (url + token for this webview) and its lifetime (tray, band).
        "local_host",
        "local_start",
        "local_status",
        "local_shutdown",
        "local_restart",
        "local_drain_cancel",
        "logs_open", // Opens the desktop directory; dexd has no server-side GUI.
        // S4: the desktop's own keyring for its hosts' device tokens — nothing to serve.
        "host_token_get",
        "host_token_set",
        "host_token_delete",
        // Region screenshot and its global hotkey drive the local desktop's screen and
        // overlay window; a remote dexd has no display to capture.
        "snip_start",
        "snip_finish",
        "snip_cancel",
        "snip_hotkey_get",
        "snip_hotkey_set",
        // #54: the usage strip reads this machine's Token Monitor hub (loopback) and keeps its
        // secret in the desktop's keyring; a remote dexd has neither.
        "usage_get",
        "usage_hub_secret_set",
    ];

    #[test]
    fn tauri_registers_only_client_only_commands() {
        let mut names = tauri_commands("core_proc::local_host");
        names.sort();
        let mut want: Vec<String> = CLIENT_ONLY.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(names, want, "Tauri'ye yalnız CLIENT_ONLY komut eklenir; çekirdek komutu dexd'ye");
        for n in CLIENT_ONLY {
            assert!(handler(n).is_none() && !LOCAL_METHODS.contains(&n), "{n} hem kabukta hem dexd'de");
        }
        // The webview's local transport sends exactly these to Tauri, the rest to the endpoint.
        let ts = include_str!("../../../src/api/clientOnly.ts");
        let list = &ts[ts.find("CLIENT_ONLY: readonly string[] = [").expect("CLIENT_ONLY")..];
        let list = &list[list.find("= [").unwrap()..list.find("];").unwrap()];
        let mut ts_names: Vec<&str> = list.split('"').skip(1).step_by(2).collect();
        ts_names.sort();
        let mut rust_names = CLIENT_ONLY.to_vec();
        rust_names.sort();
        assert_eq!(ts_names, rust_names, "src/api/clientOnly.ts ile CLIENT_ONLY ayrıştı");
    }

    /// `inproc-core` (the way back, one release): the in-app core's commands each still have a WS
    /// counterpart. The connection-bound PTY commands live in `server` (spawn needs the channel;
    /// list/attach/detach are `pty.*` sessions; #62: resize needs the connection as the size's
    /// client); the tailnet ones are the local endpoint's (`LOCAL_METHODS`).
    #[test]
    fn inproc_commands_have_a_ws_counterpart() {
        let names = tauri_commands("agents::agent_start");
        assert!(names.len() > 30, "{names:?}");
        const IN_SERVER: [&str; 7] = ["pty_spawn", "pty_list", "pty_attach", "pty_detach", "pty_resize", "server_info", "team_viewed"];
        let missing: Vec<_> = names
            .iter()
            .map(String::as_str)
            .filter(|n| !IN_SERVER.contains(n) && !LOCAL_METHODS.contains(n) && !CLIENT_ONLY.contains(n) && handler(n).is_none())
            .collect();
        assert!(missing.is_empty(), "WS karşılığı olmayan Tauri komutları: {missing:?}");
    }

    #[test]
    fn bad_arguments_are_bad_requests() {
        let root = std::env::temp_dir().join(format!("dexd-methods-args-{}", std::process::id()));
        let core = Core::start(
            Paths {
                data_dir: root.join("data"),
                config_dir: root.join("config"),
            },
            Events::new(NullSink),
        )
        .unwrap();
        let e = call(&core, "task_delete", &serde_json::json!({"id": "x"})).unwrap_err();
        assert_eq!(e.code, "bad_request");
        for params in [
            serde_json::json!({"adminToken": ["sentinel"], "limits": {}}),
            serde_json::json!({"adminToken": "sentinel", "limits": {"sentinel": 1}}),
            serde_json::json!({"adminToken": "sentinel", "limits": {"daily_image_limit": "sentinel"}}),
            serde_json::json!({"adminToken": "sentinel", "limits": {"daily_image_limit": 1}, "reason": ["sentinel"]}),
        ] {
            let e = call(&core, "gpu_limits_set", &params).unwrap_err();
            assert_eq!(e.code, "bad_request");
            assert!(!e.message.contains("sentinel"));
        }
        let status = call(&core, "core_status", &serde_json::json!({})).unwrap();
        assert_eq!(status["version"], 1);
        assert!(status["agents"].as_array().unwrap().iter().any(|a| a["agentId"] == "kaan"));
        let e = call(
            &core,
            "team_update",
            &serde_json::json!({"id": "nope", "patch": {}}),
        )
        .unwrap_err();
        assert_eq!(e.code, "failed");
        core.shutdown();
    }
}
