//! The application service over a durable profile, assembled once.
//!
//! The desktop assembled this in its own `state.rs`, and a host (ADR 0073,
//! `SHARED_PROJECTS_PLAN.md` SP1) needs exactly the same assembly: the same
//! tools under the same permission boundary, the same models, the same
//! recovery. Two copies would drift, which is the argument that put
//! `assemble_tool_plane` and the model plane in one place already.
//!
//! What differs between applications is passed in: who answers approval
//! questions, and which store holds credentials.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tramontana_adapters_tools::{FilesystemTools, GitTools, RecipeTools};
use tramontana_core::ports::{
    AppSettingsRepository, ApprovalStore, Approver, EventStore, ModelProvider,
    ProviderSettingsRepository, SecretResolver, ToolAdmission, ToolProvider,
};
use tramontana_core::services::{Providers, Repositories, TaskService};
use tramontana_core::testing::{FakeModelProvider, InMemoryApprovalStore, assemble_tool_plane};
use tramontana_persistence::{
    Database, SqliteAppSettingsRepository, SqliteCheckpointRepository,
    SqliteConfigurationRepository, SqliteConversationRepository, SqliteDictationRepository,
    SqliteEventStore, SqlitePersonalRepository, SqliteProfileEraser, SqliteProjectRepository,
    SqliteProviderSettingsRepository, SqliteRecordRemoval, SqliteScheduleRepository,
    SqliteTaskRepository,
};
use tramontana_platform::{
    FsSkillLibrary, ProfileLease, ProjectFiles, SystemClock, SystemLocalClock, UuidIdSource,
};

use crate::{
    AppPlane, BrowserProvider, ModelPlane, Supervision, assemble_models_recording, assemble_worker,
    pinnable_models, pinned_worker_choice, reap_browser_profiles, reap_runtimes, recovery_store,
    runtime_records, start_apps, worker_choice,
};

/// Everything an application holds once the profile is open.
pub struct ServicePlane {
    /// The application service every command delegates to.
    pub service: TaskService,
    /// Where grants live, so they can be listed and revoked.
    pub approvals: Arc<InMemoryApprovalStore>,
    /// Runtimes this process started; the application stops them on exit.
    pub models: ModelPlane,
    /// The Apps this process started, stopped with it (ADR 0036).
    pub apps: AppPlane,
    /// The built-in browser, stopped with the application (ADR 0047).
    pub browser: Arc<BrowserProvider>,
    /// The profile's App settings.
    pub app_settings: Arc<SqliteAppSettingsRepository>,
    /// The profile's configured endpoints.
    pub settings: Arc<SqliteProviderSettingsRepository>,
    /// Empties the profile's records (ADR 0068).
    pub eraser: Arc<SqliteProfileEraser>,
    /// Where the person's own skills are kept (ADR 0070).
    pub skills_folder: PathBuf,
}

impl std::fmt::Debug for ServicePlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServicePlane")
            .field("models", &self.models)
            .field("skills_folder", &self.skills_folder)
            .finish_non_exhaustive()
    }
}

/// How long a starting application waits for the profile's previous owner
/// to let go of it.
///
/// A restart starts the new process a moment before the old one has exited,
/// so a lease still held for that moment is expected. One held for longer
/// belongs to another process, and this one opens without running tasks.
const LEASE_PATIENCE: Duration = Duration::from_secs(3);

/// Open (and migrate) the profile database, creating its folder first.
///
/// # Errors
///
/// A message naming what could not be created or opened.
pub async fn open_database(path: &Path) -> Result<Database, String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    Database::open(path)
        .await
        .map_err(|error| format!("could not open {}: {error}", path.display()))
}

/// Open the profile at `path` and assemble the service over it.
///
/// Model providers come from the endpoints configured in this profile. A
/// profile with nothing configured falls back to the deterministic fake,
/// streamed `chunk_delay_ms` apart, and the interface says so rather than
/// letting a stub pass for a model.
///
/// # Errors
///
/// A message naming what could not be opened, read or recovered.
pub async fn open_service_plane(
    path: &Path,
    approver: Arc<dyn Approver>,
    resolver: Arc<dyn SecretResolver>,
    chunk_delay_ms: u64,
) -> Result<ServicePlane, String> {
    let database = open_database(path).await?;
    // Filesystem and Git tools, behind the permission boundary. The registry
    // says a tool exists; only the permission manager says it may run. The
    // CLI assembles the identical set, including the same recovery store
    // beside the profile, so the adapters cannot drift into offering
    // different tools under different policy.
    let approvals = Arc::new(InMemoryApprovalStore::new());
    // The profile's Apps, started before the registry is built so their tools
    // are registered beside the built-in ones and under the same permission
    // boundary (ADR 0036). A broken App is a warning, never a reason the
    // application cannot open.
    let app_settings = Arc::new(SqliteAppSettingsRepository::new(database.clone()));
    let configured_apps = app_settings
        .list()
        .await
        .map_err(|error| format!("could not read App settings: {error}"))?;
    let apps = start_apps(&configured_apps, &resolver, path).await;
    for failure in &apps.failures {
        tracing::warn!(
            app = failure.app_id,
            "an App did not start: {}",
            failure.reason
        );
    }
    let app_tools = apps.tool_ids();
    let browser = crate::browser(path);
    let mut providers: Vec<Arc<dyn ToolProvider>> = vec![
        Arc::new(FilesystemTools::new().with_recovery_store(recovery_store(path)))
            as Arc<dyn ToolProvider>,
        Arc::new(GitTools::new()) as Arc<dyn ToolProvider>,
        // Registered so every adapter holds the same set; offered to a model
        // only in a task whose person chose to allow recipes.
        Arc::new(RecipeTools::new()) as Arc<dyn ToolProvider>,
        // Likewise the browser, started only when a task that may browse
        // first opens a page (ADR 0047).
        Arc::clone(&browser) as Arc<dyn ToolProvider>,
    ];
    providers.extend(apps.providers());
    let (registry, permissions, releases) = assemble_tool_plane(
        providers,
        Arc::clone(&approvals) as Arc<dyn ApprovalStore>,
        approver,
        Arc::new(UuidIdSource::new()),
    )
    .map_err(|error| format!("could not register tools: {error}"))?;

    let lease = acquire_lease(path, LEASE_PATIENCE)
        .await
        .map_err(|error| format!("could not acquire runtime profile lease: {error}"))?
        .map(|lease| Arc::new(lease) as Arc<dyn Send + Sync>);
    // Holding the lease proves any runtime recorded for this profile outlived
    // the process that started it: stop those before starting more. Without
    // the lease they may be in use (ADR 0030).
    if lease.is_some() {
        for note in reap_runtimes(path).await {
            tracing::warn!("{note}");
        }
        reap_browser_profiles(path);
    }
    let records = lease.is_some().then(|| runtime_records(path));
    let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::new(database.clone()));
    // The endpoints the person configured. Supervised runtimes are started
    // here because the application is long-lived; the plane owns them and
    // stops them on close.
    let settings = Arc::new(SqliteProviderSettingsRepository::new(database.clone()));
    let configured = settings
        .list()
        .await
        .map_err(|error| format!("could not read provider settings: {error}"))?;
    let models_plane = assemble_models_recording(
        &configured,
        &resolver,
        Supervision::Start,
        records.as_deref(),
    )
    .await;
    for failure in &models_plane.failures {
        tracing::warn!(
            endpoint = failure.endpoint_id,
            "a configured model endpoint is unavailable: {}",
            failure.error.message
        );
    }
    let has_configured = configured.iter().any(|endpoint| endpoint.enabled);
    let models: Vec<Arc<dyn ModelProvider>> = if has_configured {
        models_plane.providers.clone()
    } else {
        // Labelled explicitly in the interface, not silently substituted.
        vec![Arc::new(FakeModelProvider::streaming_paced(
            ["Hello", ", ", "world", " from ", "Tramontana"],
            chunk_delay_ms,
        ))]
    };

    let service = TaskService::new(
        Repositories {
            projects: Arc::new(SqliteProjectRepository::new(database.clone())),
            conversations: Arc::new(SqliteConversationRepository::new(database.clone())),
            tasks: Arc::new(SqliteTaskRepository::new(database.clone())),
            events: store,
            checkpoints: Arc::new(SqliteCheckpointRepository::new(database.clone())),
            configuration: Arc::new(SqliteConfigurationRepository::new(database.clone())),
        },
        Providers {
            models,
            tools: Arc::clone(&registry),
            permissions: Arc::clone(&permissions),
            releases: Arc::clone(&releases),
            // Same strategy and governed tool plane as the CLI. The service's
            // own worker is read-only. A task gets a worker that may ask to
            // write or run recipes only when the person chose that in the
            // composer, and each one is still approved as it happens (ADRs
            // 0021 and 0031).
            worker: assemble_worker(has_configured, ToolAdmission::READ_ONLY, &[]),
        },
        Arc::new(SystemClock::new()),
        Arc::new(UuidIdSource::new()),
    );
    // The built-in worker pinned to each configured model a task can be
    // handed to (ADR 0048). None on a profile with no endpoint.
    let pinnable = if has_configured {
        pinnable_models(&models_plane.providers).await
    } else {
        Vec::new()
    };
    let service = service
        .with_worker_choice(worker_choice(has_configured, app_tools.clone()))
        .with_pinned_workers(pinnable, pinned_worker_choice(app_tools))
        // So a task is given the project's own instructions, read as it
        // starts (ADR 0052).
        .with_project_sources(Arc::new(ProjectFiles::new(Arc::new(
            SqliteProjectRepository::new(database.clone()),
        ))))
        // So chats can be told what the person wrote about themselves
        // (ADR 0061).
        .with_personal(Arc::new(SqlitePersonalRepository::new(database.clone())))
        // So removing a conversation or a project is one commit (ADR 0067).
        .with_record_removal(Arc::new(SqliteRecordRemoval::new(database.clone())))
        // So schedules run by the computer's local time (ADR 0063).
        .with_schedules(
            Arc::new(SqliteScheduleRepository::new(database.clone())),
            Arc::new(SystemLocalClock::new()),
        )
        // The person's own skills, in a folder beside the profile (ADR 0070).
        .with_skill_library(Arc::new(FsSkillLibrary::new(skills_folder(path))))
        // So speech can be transcribed on this device (ADR 0065).
        .with_dictation(
            Arc::new(SqliteDictationRepository::new(database.clone())),
            Arc::new(tramontana_adapters_local::WhisperCli::new()),
        )
        // So PDF and Word files can be attached (ADR 0062).
        .with_document_reader(Arc::new(tramontana_adapters_documents::Documents::new()))
        .with_execution_guard(lease);
    service
        .recover()
        .await
        .map_err(|error| format!("recovery failed: {}", error.message))?;
    Ok(ServicePlane {
        service,
        approvals,
        models: models_plane,
        apps,
        browser,
        app_settings,
        settings,
        eraser: Arc::new(SqliteProfileEraser::new(database)),
        skills_folder: skills_folder(path),
    })
}

/// The profile's lease, waiting up to `patience` for its holder to let go.
///
/// # Errors
///
/// When the lease file cannot be opened.
pub async fn acquire_lease(
    path: &Path,
    patience: Duration,
) -> std::io::Result<Option<ProfileLease>> {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        if let Some(lease) = ProfileLease::try_acquire(path)? {
            return Ok(Some(lease));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The person's skills live in a folder beside the profile (ADR 0070).
fn skills_folder(profile: &Path) -> PathBuf {
    profile
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("skills")
}
