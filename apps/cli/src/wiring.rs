//! Composition root for the CLI.
//!
//! Assembles storage, providers, and platform ports into a service. The desktop
//! shell assembles the identical set; if the two ever diverge, the CLI stops
//! being a faithful way to inspect what the application actually did.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tramontana_composition::{PlaneOptions, ServicePlane, Supervision};
use tramontana_core::contracts::{CorrelationId, ErrorCode, ErrorEnvelope};
use tramontana_core::ports::{AgentProvider, ToolAdmission};
use tramontana_core::services::{ErasureReport, TaskService};
use tramontana_core::testing::ReceivingWorker;
use tramontana_platform::PlatformSecretVault;

use crate::approval::TerminalApprover;

/// Where a development profile keeps its database.
///
/// Under the repository's ignored local-data directory rather than a real
/// application-support path: M2 has no installed application yet, and writing
/// to a user's system directory before there is one to uninstall would leave
/// litter nobody asked for.
#[must_use]
pub(crate) fn default_profile_path() -> PathBuf {
    PathBuf::from(".tramontana-local").join("tramontana.sqlite")
}

/// Select the profile named explicitly by the caller, then the shared profile
/// environment variable, then the repository-local development default.
///
/// `TRAMONTANA_PROFILE` gives the CLI and desktop shell a deliberate way to
/// open the same durable record during M2 validation without baking a
/// machine-specific application-data path into either adapter.
#[must_use]
pub(crate) fn profile_path(explicit: Option<PathBuf>) -> PathBuf {
    explicit
        .or_else(|| std::env::var_os("TRAMONTANA_PROFILE").map(PathBuf::from))
        .unwrap_or_else(default_profile_path)
}

/// `--profile`, for commands that take nothing else.
#[derive(Debug, clap::Args)]
pub(crate) struct ProfileArgs {
    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    pub(crate) profile: Option<PathBuf>,
}

/// Open a profile for a command that never runs a task.
///
/// Supervised runtimes are not started: listing projects must not load a
/// model. Endpoints that are already listening still come up, so a task
/// started from such a command would still route to them.
pub(crate) async fn open(path: &Path, chunk_delay_ms: u64) -> Result<Arc<TaskService>, String> {
    // Nothing supervised, so there is no runtime to keep alive and the plane
    // can be dropped here.
    open_with(
        path,
        chunk_delay_ms,
        Supervision::Skip,
        ToolAdmission::READ_ONLY,
    )
    .await
    .map(|plane| plane.service)
}

/// [`open`] the profile, or print why not; `None` means exit with failure.
pub(crate) async fn open_or_report(profile: Option<PathBuf>) -> Option<Arc<TaskService>> {
    match open(&profile_path(profile), 0).await {
        Ok(service) => Some(service),
        Err(message) => {
            eprintln!("error: {message}");
            None
        }
    }
}

/// Open a profile for a command that will actually run a model, or print
/// why not; `None` means exit with failure.
///
/// Returns the plane: it owns any runtime this process started, and
/// dropping it stops them. The caller has to hold it for as long as the task
/// runs, which is why this cannot simply return the service.
///
/// `admission` is what the reference worker may offer its model, and comes
/// from the command line: writes only when the person asked for them.
pub(crate) async fn open_running_or_report(
    profile: Option<PathBuf>,
    chunk_delay_ms: u64,
    admission: ToolAdmission,
) -> Option<ServicePlane> {
    open_with(
        &profile_path(profile),
        chunk_delay_ms,
        Supervision::Start,
        admission,
    )
    .await
    .inspect_err(|message| eprintln!("error: {message}"))
    .ok()
}

/// The shared service plane, with a terminal approver and the platform
/// credential store, and what it noticed printed to stderr: this process
/// installs no log subscriber.
async fn open_with(
    path: &Path,
    chunk_delay_ms: u64,
    supervision: Supervision,
    admission: ToolAdmission,
) -> Result<ServicePlane, String> {
    let mut plane = tramontana_composition::open_service_plane(
        path,
        // Prompts on a terminal, and refuses when there is no terminal to
        // prompt on. A read inside an approved project is permitted without
        // asking; a write or a recipe is asked about every time it runs, and
        // is offered to a model at all only under `--allow-writes` or
        // `--allow-recipes`.
        Arc::new(TerminalApprover::new()),
        Arc::new(PlatformSecretVault::new()),
        chunk_delay_ms,
        // Without the lease at once, another process owns the profile: this
        // command runs no task rather than waiting on it.
        PlaneOptions {
            supervision,
            admission,
            lease_patience: Duration::ZERO,
        },
    )
    .await?;
    for failure in &plane.apps.failures {
        eprintln!(
            "warning: App {:?} ({}) did not start: {}",
            failure.display_name, failure.app_id, failure.reason
        );
    }
    for note in &plane.reaped {
        eprintln!("note: {note}");
    }
    let models = plane.models.get_mut();
    for failure in &models.failures {
        eprintln!(
            "warning: endpoint {:?} ({}) is unavailable: {}",
            failure.display_name, failure.endpoint_id, failure.error.message
        );
    }
    // Said every time: an interface that let a deterministic stub pass for a
    // model would be lying about the one thing this project is built to be
    // honest about.
    if plane.fake_models {
        if !models.skipped.is_empty() {
            eprintln!(
                "note: {} supervised runtime(s) were not started for this command",
                models.skipped.len()
            );
        }
        eprintln!(
            "note: no model endpoint is configured, so this profile is using the \
             deterministic fake. Add one with `tramontana provider add`."
        );
    }
    if plane.recovered > 0 {
        eprintln!(
            "recovered {} task projection(s) from the event stream",
            plane.recovered
        );
    }
    Ok(plane)
}

/// Delete everything a profile holds (ADR 0068): its records, the
/// credentials they name, and the folders beside its database. The same
/// steps the desktop takes, through the same plane.
///
/// # Errors
///
/// A message when the profile cannot be opened, a task is running or
/// another process owns the profile, or a step fails.
pub(crate) async fn erase(path: &Path) -> Result<ErasureReport, ErrorEnvelope> {
    let plane = open_with(path, 0, Supervision::Skip, ToolAdmission::READ_ONLY)
        .await
        .map_err(|message| {
            ErrorEnvelope::new(ErrorCode::Internal, message, CorrelationId::new("erase"))
        })?;
    plane
        .erase_everything(path, &PlatformSecretVault::new())
        .await
}

/// Workers a task can be handed to by name.
///
/// Explicit built-in choices; external worker discovery belongs to M10.
#[must_use]
pub(crate) fn worker_named(agent_id: &str) -> Option<Arc<dyn AgentProvider>> {
    match agent_id {
        // A worker taking a task over reads: handing a task on is not a way
        // for it to gain writes nobody gave it.
        tramontana_core::services::REFERENCE_AGENT_ID => Some(
            tramontana_composition::assemble_worker(true, ToolAdmission::READ_ONLY, &[]),
        ),
        tramontana_core::testing::RECEIVING_AGENT_ID => {
            // M4 diagnostic handoff target. Its fixed step matches the fake
            // diagnostic worker, not arbitrary model-selected reference work.
            Some(Arc::new(ReceivingWorker::calling(
                "builtin.fs.list",
                serde_json::json!({ "path": "." }),
            )) as Arc<dyn AgentProvider>)
        }
        _ => None,
    }
}

/// The names [`worker_named`] accepts, for an error message worth reading.
#[must_use]
pub(crate) fn available_workers() -> Vec<&'static str> {
    vec![
        tramontana_core::services::REFERENCE_AGENT_ID,
        tramontana_core::testing::RECEIVING_AGENT_ID,
    ]
}
