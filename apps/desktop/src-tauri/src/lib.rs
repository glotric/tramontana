//! Tauri shell and composition root.
//!
//! This crate owns window and platform lifecycle and wires adapters to
//! `tramontana-core`. It holds no routing policy, permission policy, secret
//! storage, or provider logic (`docs/ARCHITECTURE.md` §2): every command here
//! validates its input, calls an application service, and translates the
//! result. The CLI is its peer over the same services.

mod commands;
mod state;

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
        .manage(state::AppState::with_fakes())
        .invoke_handler(tauri::generate_handler![
            commands::core_info,
            commands::start_task,
            commands::cancel_task,
            commands::get_task,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start the Tramontana desktop shell");
}
