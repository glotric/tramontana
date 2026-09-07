//! Tauri shell and composition root.
//!
//! This crate owns window and platform lifecycle and wires adapters to
//! `tramontana-core`. It holds no routing policy, permission policy, secret
//! storage, or provider logic (`docs/ARCHITECTURE.md` §2), and its commands
//! stay thin, validated, and versioned (`docs/DEVELOPMENT.md` §9).
//!
//! M0 exposes exactly one command, enough to prove the IPC path is real.

use serde::Serialize;

/// Payload returned by [`core_info`].
///
/// Carries `schemaVersion` like every IPC payload, so the interface can refuse
/// an incompatible core rather than misreading its fields.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreInfo {
    /// Contract schema version reported by the running core.
    pub schema_version: u32,
    /// Version of the desktop shell, from its Cargo manifest.
    pub app_version: &'static str,
}

/// Report the contract schema and application version.
///
/// The M0 walking proof that the interface, the Tauri adapter, and
/// `tramontana-core` are one process. It reads no files, opens no sockets, and
/// needs no permission decision.
#[tauri::command]
fn core_info() -> CoreInfo {
    CoreInfo {
        schema_version: tramontana_core::SCHEMA_VERSION,
        app_version: env!("CARGO_PKG_VERSION"),
    }
}

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
        .invoke_handler(tauri::generate_handler![core_info])
        .run(tauri::generate_context!())
        .expect("failed to start the Tramontana desktop shell");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_info_reports_the_core_schema_version() {
        let info = core_info();
        assert_eq!(info.schema_version, tramontana_core::SCHEMA_VERSION);
        assert!(!info.app_version.is_empty());
    }
}
