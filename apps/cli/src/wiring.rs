//! Composition root for the CLI.
//!
//! Assembles storage, providers, and platform ports into a service. The desktop
//! shell assembles the identical set; if the two ever diverge, the CLI stops
//! being a faithful way to inspect what the application actually did.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tramontana_adapters_tools::{FilesystemTools, GitTools, RecipeTools};
use tramontana_composition::{AppPlane, ModelPlane, Supervision, assemble_models_recording};
use tramontana_core::ports::AgentProvider;
use tramontana_core::ports::EventStore;
use tramontana_core::ports::{
    AppSettingsRepository, ApprovalStore, Approver, ModelProvider, ProviderSettingsRepository,
    SecretResolver, ToolProvider,
};
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{
    FakeModelProvider, InMemoryApprovalStore, ReceivingWorker, assemble_tool_plane,
};

use crate::approval::TerminalApprover;
use tramontana_persistence::{
    Database, SqliteAppSettingsRepository, SqliteConfigurationRepository,
    SqliteConversationRepository, SqliteEventStore, SqliteProjectRepository,
    SqliteProviderSettingsRepository, SqliteTaskRepository,
};
use tramontana_platform::{PlatformSecretVault, SystemClock, UuidIdSource};

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

/// Open a profile and assemble the service over it.
///
/// # Errors
///
/// Returns a message describing what could not be opened.
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
        tramontana_core::ports::ToolAdmission::READ_ONLY,
    )
    .await
    .map(|(service, _plane, _apps)| service)
}

/// Open a profile for a command that will actually run a model.
///
/// Returns the plane as well: it owns any runtime this process started, and
/// dropping it stops them. The caller has to hold it for as long as the task
/// runs, which is why this cannot simply return the service.
///
/// `admission` is what the reference worker may offer its model, and comes
/// from the command line: writes only when the person asked for them.
pub(crate) async fn open_running(
    path: &Path,
    chunk_delay_ms: u64,
    admission: tramontana_core::ports::ToolAdmission,
) -> Result<(Arc<TaskService>, ModelPlane, AppPlane), String> {
    open_with(path, chunk_delay_ms, Supervision::Start, admission).await
}

async fn open_with(
    path: &Path,
    chunk_delay_ms: u64,
    supervision: Supervision,
    admission: tramontana_core::ports::ToolAdmission,
) -> Result<(Arc<TaskService>, ModelPlane, AppPlane), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }

    let database = Database::open(path)
        .await
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;

    // The profile's Apps, started before the registry is built so their tools
    // are registered beside the built-in ones and under the same permission
    // boundary (ADR 0036). A broken App is a note, never a reason the command
    // cannot run.
    let resolver = Arc::new(PlatformSecretVault::new()) as Arc<dyn SecretResolver>;
    let configured_apps = SqliteAppSettingsRepository::new(database.clone())
        .list()
        .await
        .map_err(|error| format!("could not read App settings: {error}"))?;
    let apps = tramontana_composition::start_apps(&configured_apps, &resolver, path).await;
    for failure in &apps.failures {
        eprintln!(
            "warning: App {:?} ({}) did not start: {}",
            failure.display_name, failure.app_id, failure.reason
        );
    }
    let app_tools = apps.tool_ids();

    // Filesystem and Git tools, behind the permission boundary. The registry
    // says a tool exists; only the permission manager says it may run.
    let mut providers: Vec<Arc<dyn ToolProvider>> = vec![
        // Deleted files are kept beside the profile, so a delete can be undone.
        Arc::new(
            FilesystemTools::new()
                .with_recovery_store(tramontana_composition::recovery_store(path)),
        ) as Arc<dyn ToolProvider>,
        Arc::new(GitTools::new()) as Arc<dyn ToolProvider>,
        // The project's own commands, run only as the core resolved them.
        Arc::new(RecipeTools::new()) as Arc<dyn ToolProvider>,
        // The browser, started only when a task under `--browse` first opens
        // a page (ADR 0047).
        tramontana_composition::browser(path) as Arc<dyn ToolProvider>,
    ];
    providers.extend(apps.providers());
    let (registry, permissions, releases) = assemble_tool_plane(
        providers,
        Arc::new(InMemoryApprovalStore::new()) as Arc<dyn ApprovalStore>,
        // Prompts on a terminal, and refuses when there is no terminal to
        // prompt on. A read inside an approved project is permitted without
        // asking; a write or a recipe is asked about every time it runs, and
        // is offered to a model at all only under `--allow-writes` or
        // `--allow-recipes`.
        Arc::new(TerminalApprover::new()) as Arc<dyn Approver>,
        Arc::new(UuidIdSource::new()),
    )
    .map_err(|error| format!("could not register tools: {error}"))?;

    let lease = tramontana_platform::ProfileLease::try_acquire(path)
        .map_err(|error| format!("could not acquire runtime profile lease: {error}"))?
        .map(|lease| Arc::new(lease) as Arc<dyn Send + Sync>);
    // Holding the lease proves any runtime recorded for this profile outlived
    // the process that started it: stop those before starting more. Without
    // the lease they may be in use, so nothing is stopped or recorded
    // (ADR 0030).
    if lease.is_some() {
        for note in tramontana_composition::reap_runtimes(path).await {
            eprintln!("note: {note}");
        }
        tramontana_composition::reap_browser_profiles(path);
    }
    let records = lease
        .is_some()
        .then(|| tramontana_composition::runtime_records(path));
    let events = Arc::new(SqliteEventStore::new(database.clone())) as Arc<dyn EventStore>;

    // Real providers, from what the user configured. Credentials are resolved
    // per request out of the platform store; only opaque references are in
    // the database (ADR 0008).
    let settings = SqliteProviderSettingsRepository::new(database.clone());
    let configured = settings
        .list()
        .await
        .map_err(|error| format!("could not read provider settings: {error}"))?;
    let plane =
        assemble_models_recording(&configured, &resolver, supervision, records.as_deref()).await;

    for failure in &plane.failures {
        eprintln!(
            "warning: endpoint {:?} ({}) is unavailable: {}",
            failure.display_name, failure.endpoint_id, failure.error.message
        );
    }

    // Falling back to the fake keeps the walking skeleton usable on a profile
    // with nothing configured, and it says so every time: an interface that
    // let a deterministic stub pass for a model would be lying about the one
    // thing this project is built to be honest about.
    let has_configured = configured.iter().any(|endpoint| endpoint.enabled);
    let models: Vec<Arc<dyn ModelProvider>> = if !has_configured {
        if !plane.skipped.is_empty() {
            eprintln!(
                "note: {} supervised runtime(s) were not started for this command",
                plane.skipped.len()
            );
        }
        eprintln!(
            "note: no model endpoint is configured, so this profile is using the \
             deterministic fake. Add one with `tramontana provider add`."
        );
        vec![Arc::new(FakeModelProvider::streaming_paced(
            ["Hello", ", ", "world"],
            chunk_delay_ms,
        ))]
    } else {
        plane.providers.clone()
    };

    let service = TaskService::new(
        Repositories {
            projects: Arc::new(SqliteProjectRepository::new(database.clone())),
            conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
            tasks: Arc::new(SqliteTaskRepository::new(database.clone())),
            events,
            checkpoints: Arc::new(tramontana_persistence::SqliteCheckpointRepository::new(
                database.clone(),
            )),
            configuration: Arc::new(SqliteConfigurationRepository::new(database.clone())),
        },
        Providers {
            models,
            tools: Arc::clone(&registry),
            permissions: Arc::clone(&permissions),
            releases: Arc::clone(&releases),
            // Shared strategy selection; both real and diagnostic work use
            // the same permission-governed built-in tools.
            worker: tramontana_composition::assemble_worker(has_configured, admission, &app_tools),
        },
        Arc::new(SystemClock::new()),
        Arc::new(UuidIdSource::new()),
    );

    // The built-in worker pinned to each configured model, which a task can
    // be handed to by name (ADR 0048).
    let pinnable = if has_configured {
        tramontana_composition::pinnable_models(&plane.providers).await
    } else {
        Vec::new()
    };
    let service = service
        .with_worker_choice(tramontana_composition::worker_choice(
            has_configured,
            app_tools.clone(),
        ))
        .with_pinned_workers(
            pinnable,
            tramontana_composition::pinned_worker_choice(app_tools.clone()),
        )
        // So a task is given the project's own instructions, read as it starts
        // (ADR 0052).
        .with_project_sources(Arc::new(tramontana_platform::ProjectFiles::new(Arc::new(
            SqliteProjectRepository::new(database.clone()),
        ))))
        // So chats can be told what the person wrote about themselves
        // (ADR 0061).
        .with_personal(Arc::new(
            tramontana_persistence::SqlitePersonalRepository::new(database.clone()),
        ))
        // So schedules run by the computer's local time (ADR 0063).
        .with_schedules(
            Arc::new(tramontana_persistence::SqliteScheduleRepository::new(
                database.clone(),
            )),
            Arc::new(tramontana_platform::SystemLocalClock::new()),
        )
        // So PDF and Word files can be attached (ADR 0062).
        .with_document_reader(Arc::new(tramontana_adapters_documents::Documents::new()))
        .with_execution_guard(lease);

    // A process that died between appending an event and updating a task's
    // cached row leaves the two disagreeing. The events are the record, so the
    // cache is repaired before anything reads it.
    let repaired = service
        .recover()
        .await
        .map_err(|error| format!("recovery failed: {}", error.message))?;
    if repaired > 0 {
        eprintln!("recovered {repaired} task projection(s) from the event stream");
    }

    Ok((Arc::new(service), plane, apps))
}

/// Workers a task can be handed to by name.
///
/// Explicit built-in choices; external worker discovery belongs to M10.
#[must_use]
pub(crate) fn worker_named(agent_id: &str) -> Option<Arc<dyn AgentProvider>> {
    match agent_id {
        // A worker taking a task over reads: handing a task on is not a way
        // for it to gain writes nobody gave it.
        tramontana_core::services::REFERENCE_AGENT_ID => {
            Some(tramontana_composition::assemble_worker(
                true,
                tramontana_core::ports::ToolAdmission::READ_ONLY,
                &[],
            ))
        }
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
