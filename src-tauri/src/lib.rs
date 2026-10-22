// S5-A: the shell is split by target. Mobile (Android) ships no core — every team lives on a
// `dexd` host over WebSocket — and keeps only the device-token bridge (src-tauri/src/mobile_hosts.rs
// → Android Keystore) plus notifications.
// #150 (B): on desktop the core is `dexd --local`, its own process (core_proc.rs); the shell keeps
// only what is this machine's screen, keyring and window (src/api/clientOnly.ts). Feature
// `inproc-core` = the previous build: the core inside the app (host.rs) and its commands.
#[cfg(all(desktop, feature = "inproc-core"))]
mod agents;
#[cfg(all(desktop, feature = "inproc-core"))]
mod chat;
#[cfg(all(desktop, not(feature = "inproc-core")))]
mod core_proc;
#[cfg(all(desktop, feature = "inproc-core"))]
mod memory;
#[cfg(all(desktop, feature = "inproc-core"))]
mod db;
#[cfg(all(desktop, feature = "inproc-core"))]
mod delegate;
#[cfg(all(desktop, feature = "inproc-core"))]
mod host;
#[cfg(desktop)]
mod hosts;
#[cfg(desktop)]
mod app_paths;
// Shared storage selection; the default shell does not link the core.
#[cfg(desktop)]
#[path = "../../crates/dex-core/src/desktop_storage.rs"]
mod desktop_storage;
#[cfg(all(desktop, feature = "inproc-core"))]
mod layout;
#[cfg(all(desktop, feature = "inproc-core"))]
mod local;
#[cfg(desktop)]
mod logs;
#[cfg(all(desktop, feature = "inproc-core"))]
mod notes;
#[cfg(all(desktop, feature = "inproc-core"))]
mod catchup;
#[cfg(all(desktop, feature = "inproc-core"))]
mod plan;
#[cfg(all(desktop, feature = "inproc-core"))]
mod providers;
#[cfg(all(desktop, feature = "inproc-core"))]
mod procs;
#[cfg(all(desktop, feature = "inproc-core"))]
mod projects;
#[cfg(all(desktop, feature = "inproc-core"))]
mod pty;
#[cfg(all(desktop, feature = "inproc-core"))]
mod roles;
#[cfg(desktop)]
mod secrets;
#[cfg(all(desktop, feature = "inproc-core"))]
mod settings;
#[cfg(desktop)]
mod popout;
#[cfg(desktop)]
mod snip;
#[cfg(desktop)]
mod snip_hotkey;
#[cfg(all(desktop, feature = "inproc-core"))]
mod teams;
// #151: the core's tray icon — only with the core in its own process (desktop: Android has none).
#[cfg(all(desktop, not(feature = "inproc-core")))]
mod tray;
#[cfg(all(desktop, feature = "inproc-core"))]
mod tailnet;
#[cfg(mobile)]
mod mobile_hosts;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg_attr(mobile, allow(unused_mut))]
    let mut context = tauri::generate_context!();
    #[cfg(desktop)]
    let storage = app_paths::prepare(context.config_mut()).expect("desktop storage paths unavailable");
    // #203: before any thread — what `cargo run` set is ours, not our children's.
    #[cfg(all(desktop, not(feature = "inproc-core")))]
    core_proc::take_cargo_env();
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init());
    #[cfg(desktop)]
    let builder = builder.manage(storage);
    #[cfg(mobile)]
    let builder = builder
        .plugin(mobile_hosts::init())
        .plugin(mobile_hosts::init_share())
        .plugin(mobile_hosts::init_push());
    // #41: first, so a second launch exits before `host::plugin` starts a Core on the same dex.db;
    // the running window comes forward instead. (The Core's own dex.lock is the hard guard.)
    // #150: with the core in dexd a second window is harmless — `tauri dev` opens next to the
    // release (patron: both on the same dexd); a second release launch still focuses the first.
    #[cfg(all(desktop, any(feature = "inproc-core", not(debug_assertions))))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _, _| {
        // #151: the window may be closed (the app lives in the tray): built again.
        #[cfg(not(feature = "inproc-core"))]
        tray::open_window(app);
        #[cfg(feature = "inproc-core")]
        if let Some(w) = tauri::Manager::get_webview_window(app, "main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }));
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(snip_hotkey::plugin())
        // #2258: leader pop-out windows go with the main window; a closed one is reported to it.
        .on_window_event(popout::on_window_event)
        .setup(|app| {
            snip_hotkey::init(app.handle())?;
            #[cfg(not(feature = "inproc-core"))]
            tray::init(app.handle())?;
            Ok(())
        });
    // #150: only what the shell itself does — crates/dexd/src/methods.rs `covers_every_tauri_command`
    // keeps this list equal to CLIENT_ONLY (src/api/clientOnly.ts); everything else is dexd's.
    #[cfg(all(desktop, not(feature = "inproc-core")))]
    let builder = builder.plugin(core_proc::plugin()).invoke_handler(tauri::generate_handler![
        core_proc::local_host,
        core_proc::local_start,
        core_proc::local_proof,
        core_proc::local_status,
        core_proc::local_shutdown,
        core_proc::local_restart,
        core_proc::local_drain_cancel,
        logs::logs_open,
        hosts::host_token_get,
        hosts::host_token_set,
        hosts::host_token_delete,
        snip::snip_start,
        snip::snip_finish,
        snip::snip_cancel,
        snip_hotkey::snip_hotkey_get,
        snip_hotkey::snip_hotkey_set
    ]);
    #[cfg(all(desktop, feature = "inproc-core"))]
    let builder = builder
        .plugin(host::plugin())
        .invoke_handler(tauri::generate_handler![
            agents::agent_start,
            agents::inbox_submit,
            agents::agent_status_list,
            chat::chat_history,
            chat::usage_summary,
            chat::agent_context,
            chat::team_context,
            chat::usage_delegation,
            chat::settings_usage_prices,
            chat::settings_set_usage_prices,
            chat::chat_send,
            chat::chat_screen,
            memory::memory_graph,
            memory::memory_read,
            memory::project_memory_graph,
            memory::project_memory_read,
            memory::team_memory_graph,
            memory::team_memory_read,
            agents::agent_input,
            agents::leader_reset,
            agents::agent_first_reply_retry,
            agents::agent_first_reply_cancel,
            agents::first_reply_get,
            agents::first_reply_set,
            agents::chain_get,
            agents::chain_set_order,
            agents::chain_set_disabled,
            agents::chain_set_agent,
            agents::chain_back,
            pty::pty_spawn,
            pty::pty_list,
            pty::pty_attach,
            pty::pty_detach,
            pty::pty_write,
            pty::pty_resize,
            pty::pty_kill,
            db::task_list,
            db::task_create,
            db::task_update,
            db::task_delete,
            layout::layout_load,
            layout::layout_save,
            teams::team_list,
            teams::team_viewed,
            teams::team_create,
            teams::team_folder_preview,
            teams::team_clone_template,
            teams::team_update,
            teams::team_delete,
            teams::team_delete_info,
            teams::agent_list,
            teams::agent_create,
            teams::agent_update,
            teams::agent_delete,
            teams::team_set_workers_model,
            teams::agent_apply_pending,
            roles::role_list,
            delegate::delegation_list,
            delegate::delegation_ack,
            delegate::delegation_session_exited,
            delegate::delegation_seen,
            delegate::delegation_stalled,
            delegate::delegation_progress,
            delegate::question_list,
            delegate::question_answer,
            providers::provider_list,
            providers::provider_import_routes,
            providers::provider_set_token,
            providers::provider_local_set_base_url,
            providers::provider_local_status,
            providers::provider_local_add,
            providers::provider_local_delete,
            providers::model_news,
            providers::model_news_dismiss,
            plan::plan_resume,
            plan::plan_list,
            plan::proposal_list,
            plan::proposal_decide,
            procs::agent_processes,
            procs::kill_agent_process,
            projects::project_list,
            projects::project_create,
            projects::project_update,
            projects::project_archive,
            projects::project_reopen,
            projects::project_set_default,
            projects::project_context,
            projects::project_verify,
            projects::session_projects,
            settings::limits_get,
            settings::core_status,
            logs::logs_open,
            settings::limits_set,
            tailnet::tailnet_get,
            tailnet::tailnet_set,
            tailnet::tailnet_pair_code,
            tailnet::tailnet_devices,
            tailnet::tailnet_device_revoke,
            notes::note_get,
            notes::note_set,
            catchup::catchup,
            catchup::catchup_ack,
            hosts::host_token_get,
            hosts::host_token_set,
            hosts::host_token_delete,
            hosts::server_info,
            local::local_host,
            snip::snip_start,
            snip::snip_finish,
            snip::snip_cancel,
            snip_hotkey::snip_hotkey_get,
            snip_hotkey::snip_hotkey_set
        ]);
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        mobile_hosts::host_token_get,
        mobile_hosts::host_token_set,
        mobile_hosts::host_token_delete,
        mobile_hosts::share_take,
        mobile_hosts::share_opaque,
        mobile_hosts::share_done,
        mobile_hosts::push_token,
        mobile_hosts::push_take,
        mobile_hosts::push_open_url
    ]);
    let app = builder.build(context).expect("error while running tauri application");
    app.run(|_app, _e| {
        // #151: the last window closed → the app stays in the tray for the core's grace (tray.rs).
        #[cfg(all(desktop, not(feature = "inproc-core")))]
        if let tauri::RunEvent::ExitRequested { code: None, api, .. } = _e {
            api.prevent_exit();
            tray::window_closed(_app);
        }
    });
}
