//! Configuring where models can be reached.
//!
//! Endpoints are profile-scoped, not project-scoped: `CONFIGURATION.md` §153
//! forbids project-authored Markdown from hard-coding a provider or embedding
//! a credential, because a repository that named its own endpoint would carry
//! a routing decision into every checkout of it.
//!
//! A credential is never given here. `--credential` names a key in the
//! operating system store, put there by `tramontana secret set`, and only
//! that opaque reference reaches the database (ADR 0008).

use std::process::ExitCode;

use clap::{Args, Subcommand};
use tramontana_core::contracts::{
    ModelLocation, ProviderConfig, ProviderEndpoint, SecretRef, SecretStore,
};
use tramontana_core::ports::Clock;
use tramontana_core::ports::ProviderSettingsRepository;
use tramontana_persistence::{Database, SqliteProviderSettingsRepository};
use tramontana_platform::SystemClock;

use crate::wiring;

/// Arguments for `tramontana provider`.
#[derive(Debug, Args)]
pub(crate) struct ProviderArgs {
    #[command(subcommand)]
    command: ProviderCommand,
}

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    /// Configure an endpoint speaking the OpenAI chat-completions format.
    ///
    /// Covers a `llama-server`, Ollama, LM Studio, vLLM, OpenAI itself, or an
    /// `mlx_lm.server` already running elsewhere.
    AddOpenai(AddOpenAiArgs),
    /// Configure Anthropic's Messages API.
    AddAnthropic(AddAnthropicArgs),
    /// Configure a `llama-server` this application starts and stops.
    AddLlama(AddLlamaArgs),
    /// Configure an `mlx_lm.server` this application starts and stops.
    ///
    /// Apple Silicon only. Elsewhere it is saved and listed, but unavailable.
    AddMlx(AddMlxArgs),
    /// List configured endpoints.
    List(ProfileArgs),
    /// Turn an endpoint on or off without losing its configuration.
    Enable(ToggleArgs),
    /// Record verified tool-call support for this model/endpoint; unknown is off.
    ToolCalls(ToggleArgs),
    /// Remove an endpoint. The credential in the OS store is left alone.
    Remove(RemoveArgs),
    /// Say whether a runtime's executable and model are still what was
    /// recorded when it was added or last confirmed. Reads the whole model.
    Check(RemoveArgs),
    /// Confirm a runtime's executable and model as they are now, after an
    /// upgrade or a changed model (ADR 0043). A changed executable is not
    /// started until this is done.
    Trust(RemoveArgs),
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
struct AddOpenAiArgs {
    /// Identifier for this endpoint, used in routing records.
    #[arg(value_name = "ID")]
    id: String,
    /// Base URL. Plain http is accepted only for loopback (ADR 0009).
    #[arg(long, value_name = "URL")]
    base_url: String,
    /// The model to ask for.
    #[arg(long, value_name = "MODEL")]
    model: String,
    /// Display name. Defaults to the identifier.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Key in the OS credential store, if this endpoint needs one.
    ///
    /// The key, never the value: put the value there with
    /// `tramontana secret set <KEY>`.
    #[arg(long, value_name = "KEY")]
    credential: Option<String>,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct AddAnthropicArgs {
    /// Identifier for this endpoint.
    #[arg(value_name = "ID")]
    id: String,
    /// The model to ask for.
    #[arg(long, value_name = "MODEL")]
    model: String,
    /// Base URL. Defaults to Anthropic's.
    #[arg(long, value_name = "URL", default_value = "https://api.anthropic.com")]
    base_url: String,
    /// Display name. Defaults to the identifier.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Key in the OS credential store. Required: the API refuses
    /// unauthenticated requests.
    #[arg(long, value_name = "KEY")]
    credential: String,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct AddLlamaArgs {
    /// Identifier for this endpoint.
    #[arg(value_name = "ID")]
    id: String,
    /// The `llama-server` executable. Never discovered from `PATH`.
    #[arg(long, value_name = "PATH")]
    binary: std::path::PathBuf,
    /// The GGUF model file.
    #[arg(long, value_name = "PATH")]
    model_path: std::path::PathBuf,
    /// The name the model is reported under.
    #[arg(long, value_name = "MODEL")]
    model: String,
    /// Display name. Defaults to the identifier.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct AddMlxArgs {
    /// Identifier for this endpoint.
    #[arg(value_name = "ID")]
    id: String,
    /// The `mlx_lm.server` executable, usually in a virtual environment you
    /// made with `pip install mlx-lm`. Never discovered from `PATH`.
    #[arg(long, value_name = "PATH")]
    binary: std::path::PathBuf,
    /// The MLX model folder: weights, tokenizer and config.
    #[arg(long, value_name = "PATH")]
    model_path: std::path::PathBuf,
    /// The name the model is reported under.
    #[arg(long, value_name = "MODEL")]
    model: String,
    /// Display name. Defaults to the identifier.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct ToggleArgs {
    /// Which endpoint.
    #[arg(value_name = "ID")]
    id: String,
    /// Turn it off rather than on.
    #[arg(long)]
    off: bool,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    /// Which endpoint.
    #[arg(value_name = "ID")]
    id: String,
    #[command(flatten)]
    profile: ProfileArgs,
}

/// Run a `provider` subcommand.
pub(crate) async fn execute(args: ProviderArgs) -> ExitCode {
    match args.command {
        ProviderCommand::AddOpenai(args) => {
            let credential = args.credential.as_deref().map(reference);
            add(
                args.profile,
                &args.id,
                args.name.clone(),
                ProviderConfig::OpenAiCompatible {
                    base_url: args.base_url.clone(),
                    model_id: args.model.clone(),
                    credential,
                },
            )
            .await
        }
        ProviderCommand::AddAnthropic(args) => {
            add(
                args.profile,
                &args.id,
                args.name.clone(),
                ProviderConfig::Anthropic {
                    base_url: args.base_url.clone(),
                    model_id: args.model.clone(),
                    credential: reference(&args.credential),
                },
            )
            .await
        }
        ProviderCommand::AddLlama(args) => {
            add(
                args.profile,
                &args.id,
                args.name.clone(),
                ProviderConfig::LlamaServer {
                    binary_path: args.binary.display().to_string(),
                    model_path: args.model_path.display().to_string(),
                    model_id: args.model.clone(),
                    extra_args: Vec::new(),
                    provenance: None,
                },
            )
            .await
        }
        ProviderCommand::AddMlx(args) => {
            add(
                args.profile,
                &args.id,
                args.name.clone(),
                ProviderConfig::MlxServer {
                    binary_path: args.binary.display().to_string(),
                    model_path: args.model_path.display().to_string(),
                    model_id: args.model.clone(),
                    extra_args: Vec::new(),
                    provenance: None,
                },
            )
            .await
        }
        ProviderCommand::List(args) => list(args).await,
        ProviderCommand::Enable(args) => toggle(args, false).await,
        ProviderCommand::ToolCalls(args) => toggle(args, true).await,
        ProviderCommand::Remove(args) => remove(args).await,
        ProviderCommand::Check(args) => check(args).await,
        ProviderCommand::Trust(args) => trust(args).await,
    }
}

/// An opaque handle to a credential in the platform store.
fn reference(key: &str) -> SecretRef {
    SecretRef {
        key: key.to_owned(),
        store: SecretStore::Platform,
    }
}

/// Open just the settings repository.
///
/// Not the whole task service: configuring an endpoint must not start a model
/// runtime, and it must work on a profile whose configured runtime is
/// currently broken.
async fn settings(profile: ProfileArgs) -> Result<SqliteProviderSettingsRepository, String> {
    let path = wiring::profile_path(profile.profile);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let database = Database::open(&path)
        .await
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    Ok(SqliteProviderSettingsRepository::new(database))
}

async fn add(
    profile: ProfileArgs,
    id: &str,
    name: Option<String>,
    config: ProviderConfig,
) -> ExitCode {
    let settings = match settings(profile).await {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    // A runtime Tramontana starts is recorded as it is now, so a later
    // change to its executable is held rather than run (ADR 0043).
    let config = match tramontana_composition::runtime_files(&config) {
        Some((binary, model, _)) => {
            match tramontana_composition::record_provenance(
                std::path::Path::new(binary),
                std::path::Path::new(model),
                SystemClock::new().now(),
            )
            .await
            {
                Ok(recorded) => tramontana_composition::with_provenance(config.clone(), recorded),
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        None => config,
    };

    let endpoint = ProviderEndpoint::new(
        id,
        name.unwrap_or_else(|| id.to_owned()),
        config,
        // The same clock the durable record uses, so a settings row and an
        // event that mention the same moment agree about what it was.
        SystemClock::new().now(),
    );

    if let Err(error) = settings.save(&endpoint).await {
        eprintln!("error: could not save the endpoint: {error}");
        return ExitCode::FAILURE;
    }

    // The location is derived, and printing it is the point: a user who typed
    // a URL they thought was local should see immediately that it is not,
    // because that is what decides whether their work leaves the device.
    println!(
        "configured {} ({}) -> {} [{}]",
        endpoint.endpoint_id,
        endpoint.config.kind(),
        endpoint.config.model_id(),
        location_label(endpoint.location()),
    );
    if let Some(credential) = endpoint.config.credential() {
        println!(
            "  credential: {} (set its value with `tramontana secret set {}`)",
            credential.key, credential.key
        );
    }
    ExitCode::SUCCESS
}

async fn list(args: ProfileArgs) -> ExitCode {
    let settings = match settings(args).await {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let endpoints = match settings.list().await {
        Ok(endpoints) => endpoints,
        Err(error) => {
            eprintln!("error: could not read endpoints: {error}");
            return ExitCode::FAILURE;
        }
    };

    if endpoints.is_empty() {
        println!("no model endpoints are configured; tasks will use the deterministic fake");
        return ExitCode::SUCCESS;
    }

    for endpoint in endpoints {
        let files = match tramontana_composition::runtime_files(&endpoint.config) {
            Some((binary, _, recorded)) => {
                let check = tramontana_composition::check_executable(
                    std::path::Path::new(binary),
                    recorded,
                )
                .await;
                format!(" files={}", describe_executable(check))
            }
            None => String::new(),
        };
        println!(
            "{:<20} {:<18} {:<24} {:<7} {} tool-calls={}{files}",
            endpoint.endpoint_id,
            endpoint.config.kind(),
            endpoint.config.model_id(),
            location_label(endpoint.location()),
            if endpoint.enabled {
                "enabled"
            } else {
                "disabled"
            },
            endpoint.supports_tool_calls,
        );
    }
    ExitCode::SUCCESS
}

async fn toggle(args: ToggleArgs, capability: bool) -> ExitCode {
    let wanted = !args.off;
    let id = args.id.clone();
    let settings = match settings(args.profile).await {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut endpoints = match settings.list().await {
        Ok(endpoints) => endpoints,
        Err(error) => {
            eprintln!("error: could not read endpoints: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(endpoint) = endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == id)
    else {
        eprintln!("error: no endpoint named {id:?}");
        return ExitCode::FAILURE;
    };
    if capability {
        endpoint.supports_tool_calls = wanted;
    } else {
        endpoint.enabled = wanted;
    }
    if let Err(error) = settings.save(endpoint).await {
        eprintln!("error: could not save the endpoint: {error}");
        return ExitCode::FAILURE;
    }
    println!(
        "{id}: {} {}",
        if capability {
            "verified tool-call support"
        } else {
            "endpoint"
        },
        if wanted { "enabled" } else { "disabled" }
    );
    ExitCode::SUCCESS
}

async fn remove(args: RemoveArgs) -> ExitCode {
    let id = args.id.clone();
    let settings = match settings(args.profile).await {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    match settings.delete(&id).await {
        Ok(true) => {
            println!("{id}: removed");
            // Said explicitly. Deleting the credential too would destroy
            // something the OS store may share with other applications, and
            // silently leaving it would be a surprise later.
            println!(
                "  its credential, if any, is still in the OS store; `tramontana secret forget` removes that"
            );
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("{id}: nothing was configured");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: could not remove the endpoint: {error}");
            ExitCode::FAILURE
        }
    }
}

/// How a location reads in a listing.
fn describe_executable(check: tramontana_composition::FileCheck) -> &'static str {
    match check {
        tramontana_composition::FileCheck::Unchanged => "confirmed",
        tramontana_composition::FileCheck::Changed => "HELD(executable-changed)",
        tramontana_composition::FileCheck::Unreadable => "HELD(executable-unreadable)",
        tramontana_composition::FileCheck::NotRecorded => "not-recorded",
    }
}

/// Find one endpoint by identifier, printing why when it cannot.
async fn find(
    profile: ProfileArgs,
    id: &str,
) -> Option<(SqliteProviderSettingsRepository, ProviderEndpoint)> {
    let settings = match settings(profile).await {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return None;
        }
    };
    match settings.list().await {
        Ok(endpoints) => match endpoints
            .into_iter()
            .find(|endpoint| endpoint.endpoint_id == id)
        {
            Some(endpoint) => Some((settings, endpoint)),
            None => {
                eprintln!("error: no endpoint named {id:?}");
                None
            }
        },
        Err(error) => {
            eprintln!("error: could not read endpoints: {error}");
            None
        }
    }
}

async fn check(args: RemoveArgs) -> ExitCode {
    let Some((_, endpoint)) = find(args.profile, &args.id).await else {
        return ExitCode::FAILURE;
    };
    let Some((binary, model, recorded)) = tramontana_composition::runtime_files(&endpoint.config)
    else {
        eprintln!("error: {} is not a runtime Tramontana starts", args.id);
        return ExitCode::FAILURE;
    };
    let executable =
        tramontana_composition::check_executable(std::path::Path::new(binary), recorded).await;
    let weights = tramontana_composition::check_model(std::path::Path::new(model), recorded).await;
    println!("executable: {}", describe_executable(executable));
    println!(
        "model: {}",
        match weights {
            tramontana_composition::FileCheck::Unchanged => "as recorded",
            tramontana_composition::FileCheck::Changed => "CHANGED since it was recorded",
            tramontana_composition::FileCheck::Unreadable => "could not be read",
            tramontana_composition::FileCheck::NotRecorded => "not recorded",
        }
    );
    if let Some(recorded) = recorded {
        println!(
            "recorded {}: executable {} bytes, model {} bytes",
            recorded.recorded_at.as_str(),
            recorded.executable_bytes,
            recorded.model_bytes
        );
    }
    ExitCode::SUCCESS
}

async fn trust(args: RemoveArgs) -> ExitCode {
    let Some((settings, mut endpoint)) = find(args.profile, &args.id).await else {
        return ExitCode::FAILURE;
    };
    let Some((binary, model, _)) = tramontana_composition::runtime_files(&endpoint.config) else {
        eprintln!("error: {} is not a runtime Tramontana starts", args.id);
        return ExitCode::FAILURE;
    };
    let recorded = match tramontana_composition::record_provenance(
        std::path::Path::new(binary),
        std::path::Path::new(model),
        SystemClock::new().now(),
    )
    .await
    {
        Ok(recorded) => recorded,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    endpoint.config = tramontana_composition::with_provenance(endpoint.config.clone(), recorded);
    if let Err(error) = settings.save(&endpoint).await {
        eprintln!("error: could not save the endpoint: {error}");
        return ExitCode::FAILURE;
    }
    println!(
        "{}: executable and model confirmed as they are now; takes effect the next time Tramontana starts",
        args.id
    );
    ExitCode::SUCCESS
}

fn location_label(location: ModelLocation) -> &'static str {
    match location {
        ModelLocation::Local => "local",
        ModelLocation::Cloud => "CLOUD",
    }
}
