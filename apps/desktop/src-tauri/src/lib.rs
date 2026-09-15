//! Tauri shell and composition root.
//!
//! This crate owns window and platform lifecycle and wires adapters to
//! `tramontana-core`. It holds no routing policy, permission policy, secret
//! storage, or provider logic (`docs/ARCHITECTURE.md` §2): every command here
//! validates its input, calls an application service, and translates the
//! result. The CLI is its peer over the same services.

mod approval;
mod bundles;
mod commands;
mod settings;
mod state;

use tauri::Manager;

/// Build and run the desktop application.
///
/// # Panics
///
/// Panics if the Tauri runtime cannot start. There is no window in which to
/// show a recoverable error at that point, so failing loudly is the honest
/// outcome.
#[allow(clippy::expect_used, reason = "unrecoverable startup failure")]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let profile = std::env::var_os("TRAMONTANA_PROFILE")
                .map(std::path::PathBuf::from)
                .map_or_else(
                    || {
                        app.path()
                            .app_data_dir()
                            .map(|directory| directory.join("tramontana.sqlite"))
                    },
                    Ok,
                )?;
            // The approver needs a window handle, so state is assembled here
            // rather than before the app exists.
            let state = tauri::async_runtime::block_on(state::AppState::open(
                &profile,
                app.handle().clone(),
            ))
            .map_err(std::io::Error::other)?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::core_info,
            commands::open_project,
            commands::list_projects,
            commands::inspect_project,
            commands::create_conversation,
            commands::list_conversations,
            commands::list_tasks,
            commands::replay_task_events,
            commands::start_task,
            commands::watch_task,
            commands::cancel_task,
            commands::get_task,
            commands::create_checkpoint,
            commands::latest_checkpoint,
            commands::resume_task,
            commands::discard_checkpoint,
            commands::resolve_approval,
            commands::pending_approvals,
            commands::project_changes,
            commands::export_bundle,
            commands::inspect_bundle,
            commands::import_staged_bundle,
            commands::list_configured_endpoints,
            commands::add_openai_endpoint,
            commands::add_anthropic_endpoint,
            commands::add_llama_endpoint,
            commands::set_endpoint_enabled,
            commands::set_endpoint_tool_calls,
            commands::remove_endpoint,
            commands::store_credential,
            commands::forget_credential,
            commands::restart_application,
            commands::list_project_grants,
            commands::revoke_project_grant,
            commands::list_model_endpoints,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the Tramontana desktop shell");
}
