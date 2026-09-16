//! Turning configured endpoints into live providers.
//!
//! `ARCHITECTURE.md` §60 calls the Tauri shell "the composition root", and
//! until now the CLI was a second one. Two composition roots that each
//! assemble providers can drift, and then the CLI becomes a misleading way to
//! inspect what the desktop actually did — the argument that already put
//! `assemble_tool_plane` in one place. This crate is the model half of it.
//!
//! It is the only place that knows every adapter exists. `crates/core` must
//! not (`ARCHITECTURE.md` §2), and neither app should have to.
//!
//! **A broken endpoint does not break the application.** `CONFIGURATION.md`
//! §5 already settles this for instruction files: "a broken file in one
//! project must not make the application unusable". The same applies to a
//! model runtime whose binary was moved or whose credential was revoked —
//! assembly returns the endpoints that worked and a diagnostic for each that
//! did not, rather than refusing to start.

mod apps;

use std::sync::Arc;

pub use apps::{AppFailure, AppPlane, app_folder, start_app, start_apps};
use tramontana_adapters_local::{LlamaServerSpec, SupervisedLlamaServer};
use tramontana_adapters_model::{AnthropicProvider, OpenAiCompatibleProvider, ProviderAuth};
use tramontana_core::contracts::{
    ErrorEnvelope, Health, ModelCapability, ModelDescriptor, ModelLocation, ProviderConfig,
    ProviderEndpoint,
};
use tramontana_core::ports::{ModelProvider, SecretResolver, ToolAdmission};

/// Both applications select the same worker. Empty profiles retain the labelled
/// deterministic diagnostic; configured endpoints use the actual bounded loop.
///
/// `admission` is the caller's choice, never a default: writes and recipes are
/// each offered to the model only when a person asked for them, and every one
/// is still approved as it happens (ADRs 0021 and 0025). The diagnostic worker
/// ignores it.
#[must_use]
pub fn assemble_worker(
    configured: bool,
    admission: ToolAdmission,
    app_tools: &[String],
) -> Arc<dyn tramontana_core::ports::AgentProvider> {
    if configured {
        let mut tools: Vec<String> = [
            "builtin.fs.list",
            "builtin.fs.read",
            "builtin.fs.search",
            "builtin.git.status",
            "builtin.git.diff",
            "builtin.git.log",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if admission.writes {
            tools.extend(
                [
                    "builtin.fs.create",
                    "builtin.fs.patch",
                    "builtin.fs.move",
                    "builtin.fs.delete",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        if admission.recipes {
            tools.push(tramontana_core::services::RECIPE_TOOL_ID.to_owned());
        }
        if admission.apps {
            // Tramontana's own tools first: the catalogue refuses a whole
            // selection of more than `MAX_OFFERED_TOOLS`, so an App with a
            // great many tools must not be able to crowd out the built-in
            // ones or fail the task (ADR 0036).
            let room = MAX_OFFERED_TOOLS.saturating_sub(tools.len());
            tools.extend(app_tools.iter().take(room).cloned());
        }
        Arc::new(
            tramontana_core::services::ReferenceWorker::new(
                tools,
                tramontana_core::ports::ReadOnlyLimits::default(),
            )
            .admitting(admission),
        )
    } else {
        Arc::new(tramontana_core::testing::FakeWorker::calling(
            "builtin.fs.list",
            serde_json::json!({"path":"."}),
        ))
    }
}

/// Every worker [`assemble_worker`] can build, for a service that lets each
/// task choose what its model may be offered (ADR 0031).
#[must_use]
pub fn worker_choice(
    configured: bool,
    app_tools: Vec<String>,
) -> tramontana_core::services::WorkerChoice {
    Arc::new(move |admission| assemble_worker(configured, admission, &app_tools))
}

/// The most tools one task's model may be offered.
///
/// The catalogue's own limit: a selection naming more is refused whole
/// (`ModelToolCatalogue::select`), so composition stops at it rather than
/// building a selection that would fail the task.
pub const MAX_OFFERED_TOOLS: usize = 64;

/// Where a profile keeps the files its tasks deleted, so a delete can be undone.
///
/// Beside the profile's database and named after it: one store per profile,
/// outside every project. The suffix is appended rather than swapped for the
/// extension, so no profile name can make the store and the database collide
/// (ADR 0023).
///
/// ```
/// let store = tramontana_composition::recovery_store(std::path::Path::new("data/p.sqlite"));
/// assert_eq!(store, std::path::Path::new("data/p.sqlite.recovery"));
/// let odd = tramontana_composition::recovery_store(std::path::Path::new("data/x.recovery"));
/// assert_eq!(odd, std::path::Path::new("data/x.recovery.recovery"));
/// ```
#[must_use]
pub fn recovery_store(profile: &std::path::Path) -> std::path::PathBuf {
    let mut name = profile
        .file_name()
        .map_or_else(|| std::ffi::OsString::from("profile"), ToOwned::to_owned);
    name.push(".recovery");
    profile.with_file_name(name)
}

/// Where a profile records the model runtimes running for it: beside the
/// profile, named after it (ADR 0030).
pub fn runtime_records(profile: &std::path::Path) -> std::path::PathBuf {
    let mut name = profile
        .file_name()
        .map_or_else(|| std::ffi::OsString::from("profile"), ToOwned::to_owned);
    name.push(".runtimes");
    profile.with_file_name(name)
}

/// Stop model runtimes that a previous process on this profile left running.
///
/// Call only while holding the profile's lease: that is what proves the
/// process that started them is gone (ADR 0030). Returns a line for each
/// runtime stopped, for the caller to show.
pub async fn reap_runtimes(profile: &std::path::Path) -> Vec<String> {
    tramontana_adapters_local::runtime_records::reap(&runtime_records(profile))
        .await
        .into_iter()
        .filter_map(|reaped| match reaped {
            tramontana_adapters_local::runtime_records::Reaped::Stopped(record) => Some(format!(
                "stopped a model runtime left running by an earlier process (pid {}, port {})",
                record.pid, record.port
            )),
            _ => None,
        })
        .collect()
}

/// Whether to start runtimes this process would own.
///
/// Listing projects must not load a seven-billion-parameter model. A command
/// that never runs a task asks for [`Supervision::Skip`], and a supervised
/// endpoint is reported as skipped rather than started — visible, so the
/// difference between "not configured" and "not started for this command" is
/// never a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervision {
    /// Start supervised runtimes.
    Start,
    /// Leave them alone; only endpoints that are already listening come up.
    Skip,
}

/// Why one endpoint could not be brought up.
///
/// Carries the endpoint it belongs to, because "a provider failed" is not
/// actionable and "the llama-server for *Local Llama 3* is not where you left
/// it" is.
#[derive(Debug, Clone)]
pub struct EndpointFailure {
    /// Which endpoint.
    pub endpoint_id: String,
    /// What to call it.
    pub display_name: String,
    /// What went wrong, already normalized and secret-free.
    pub error: ErrorEnvelope,
}

/// Every provider that came up, and every one that did not.
pub struct ModelPlane {
    /// Live providers, in configuration order.
    pub providers: Vec<Arc<dyn ModelProvider>>,
    /// Endpoints that could not be brought up.
    pub failures: Vec<EndpointFailure>,
    /// Supervised endpoints deliberately not started for this command.
    pub skipped: Vec<String>,
    /// Runtimes this process started and must stop.
    supervised: Vec<SupervisedLlamaServer>,
}

impl std::fmt::Debug for ModelPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelPlane")
            .field("providers", &self.providers.len())
            .field("failures", &self.failures.len())
            .field("skipped", &self.skipped.len())
            .field("supervised", &self.supervised.len())
            .finish()
    }
}

impl ModelPlane {
    /// Stop every runtime this process started.
    ///
    /// Providers that were only *pointed at* something — a URL the user
    /// already had running — are left alone: stopping a server we did not
    /// start would be reaching outside what we own.
    pub async fn shutdown(&mut self) {
        for runtime in &mut self.supervised {
            runtime.shutdown().await;
        }
        self.supervised.clear();
    }
}

/// Bring up every enabled endpoint.
///
/// Never fails as a whole. An endpoint that cannot be brought up becomes an
/// [`EndpointFailure`] and the rest still run, because a profile with one
/// stale runtime path should still be able to use its other models.
pub async fn assemble_models(
    endpoints: &[ProviderEndpoint],
    resolver: &Arc<dyn SecretResolver>,
    supervision: Supervision,
) -> ModelPlane {
    assemble_models_recording(endpoints, resolver, supervision, None).await
}

/// [`assemble_models`], recording each runtime it starts in `records`, so
/// the next holder of the profile can stop one left behind (ADR 0030).
pub async fn assemble_models_recording(
    endpoints: &[ProviderEndpoint],
    resolver: &Arc<dyn SecretResolver>,
    supervision: Supervision,
    records: Option<&std::path::Path>,
) -> ModelPlane {
    let mut plane = ModelPlane {
        providers: Vec::new(),
        failures: Vec::new(),
        skipped: Vec::new(),
        supervised: Vec::new(),
    };

    for endpoint in endpoints {
        if !endpoint.enabled {
            continue;
        }
        if supervision == Supervision::Skip
            && matches!(endpoint.config, ProviderConfig::LlamaServer { .. })
        {
            plane.skipped.push(endpoint.endpoint_id.clone());
            continue;
        }
        match bring_up(endpoint, resolver, &mut plane.supervised, records).await {
            Ok(provider) => plane.providers.push(provider),
            Err(error) => {
                tracing::warn!(
                    endpoint = endpoint.endpoint_id,
                    code = ?error.code,
                    "a configured model endpoint could not be brought up"
                );
                plane.failures.push(EndpointFailure {
                    endpoint_id: endpoint.endpoint_id.clone(),
                    display_name: endpoint.display_name.clone(),
                    error,
                });
            }
        }
    }
    plane
}

/// Bring up one endpoint.
async fn bring_up(
    endpoint: &ProviderEndpoint,
    resolver: &Arc<dyn SecretResolver>,
    supervised: &mut Vec<SupervisedLlamaServer>,
    records: Option<&std::path::Path>,
) -> Result<Arc<dyn ModelProvider>, ErrorEnvelope> {
    // The configured endpoint's identifier *is* the provider identifier, so a
    // routing decision naming a provider can be resolved back to the row the
    // user edited — even when two endpoints point at the same vendor.
    let provider_id = endpoint.endpoint_id.as_str();

    match &endpoint.config {
        ProviderConfig::OpenAiCompatible {
            base_url,
            model_id,
            credential,
        } => {
            let auth = match credential {
                Some(reference) => ProviderAuth::Bearer {
                    reference: reference.clone(),
                    resolver: Arc::clone(resolver),
                },
                None => ProviderAuth::None,
            };
            let provider = OpenAiCompatibleProvider::new(
                provider_id,
                base_url,
                descriptor(endpoint, model_id, endpoint.location()),
                auth,
            )?;
            Ok(Arc::new(provider))
        }

        ProviderConfig::Anthropic {
            base_url,
            model_id,
            credential,
        } => {
            let provider = AnthropicProvider::new(
                provider_id,
                base_url,
                descriptor(endpoint, model_id, ModelLocation::Cloud),
                credential.clone(),
                Arc::clone(resolver),
            )?;
            Ok(Arc::new(provider))
        }

        ProviderConfig::LlamaServer {
            binary_path,
            model_path,
            model_id,
            extra_args,
        } => {
            let mut spec = LlamaServerSpec::new(binary_path, model_path);
            spec.extra_args = extra_args.clone();
            spec.records = records.map(std::path::Path::to_path_buf);
            let runtime = SupervisedLlamaServer::start(&spec).await?;

            // Reached over the loopback URL it was given, with the same
            // adapter every other chat-completions endpoint uses. A supervised
            // runtime is not a special kind of model, only a model whose
            // lifetime this process owns.
            let provider = OpenAiCompatibleProvider::new(
                provider_id,
                runtime.base_url(),
                descriptor(endpoint, model_id, ModelLocation::Local),
                ProviderAuth::None,
            )?;
            supervised.push(runtime);
            Ok(Arc::new(provider))
        }

        // A profile written by a newer build. Reported as an unsupported
        // endpoint rather than ignored: silently dropping it would make a
        // model the user configured simply not appear, with nothing to
        // explain why (`CORE_CONTRACTS.md` §2 reads an unknown kind as
        // unsupported, not as absent).
        unknown => Err(ErrorEnvelope::new(
            tramontana_core::contracts::ErrorCode::CapabilityUnsupported,
            format!(
                "this build does not understand the {:?} endpoint kind; it was configured by a newer version",
                unknown.kind()
            ),
            tramontana_core::contracts::CorrelationId::new("composition"),
        )),
    }
}

/// The descriptor an endpoint advertises.
///
/// `provider_id` matches the provider's own id, which the shared contract
/// suite requires: a model that claimed a different provider could not be
/// dispatched to from a routing decision.
fn descriptor(
    endpoint: &ProviderEndpoint,
    model_id: &str,
    location: ModelLocation,
) -> ModelDescriptor {
    let mut capabilities = vec![
        ModelCapability::Text,
        ModelCapability::Streaming,
        ModelCapability::Cancellation,
    ];
    if endpoint.supports_tool_calls {
        capabilities.push(ModelCapability::ToolCalls);
    }
    ModelDescriptor {
        provider_id: endpoint.endpoint_id.clone(),
        model_id: model_id.to_owned(),
        display_name: endpoint.display_name.clone(),
        location,
        capabilities,
        // Not known without asking the endpoint, and guessing a number the
        // router would then enforce is worse than admitting it is unknown:
        // `context_too_large` is a refusal, and inventing the threshold would
        // refuse work for a reason nobody chose.
        context_limit: None,
        // Assembly succeeded, which is as much as is known before a request.
        // A real check is the health probe, not this field.
        health: Health::Available,
        health_reason_code: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_capability_requires_an_explicit_endpoint_fact() {
        let mut endpoint = ProviderEndpoint::new(
            "fixture",
            "Tool-capable sounding name",
            ProviderConfig::OpenAiCompatible {
                base_url: "http://127.0.0.1:8080".into(),
                model_id: "fixture".into(),
                credential: None,
            },
            tramontana_core::contracts::Timestamp::new("2026-09-13T00:00:00Z"),
        );
        assert!(
            !descriptor(&endpoint, "fixture", ModelLocation::Local)
                .capabilities
                .contains(&ModelCapability::ToolCalls)
        );
        endpoint.supports_tool_calls = true;
        assert!(
            descriptor(&endpoint, "fixture", ModelLocation::Local)
                .capabilities
                .contains(&ModelCapability::ToolCalls)
        );
        endpoint.supports_tool_calls = false;
        assert!(
            !descriptor(&endpoint, "fixture", ModelLocation::Cloud)
                .capabilities
                .contains(&ModelCapability::ToolCalls)
        );
    }
}
