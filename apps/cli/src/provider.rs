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

use std::path::Path;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use tramontana_composition::FileCheck;
use tramontana_composition::endpoints::{self, NewEndpoint, Toggle};
use tramontana_core::contracts::{ErrorEnvelope, ModelLocation};
use tramontana_core::ports::{Clock, ProviderSettingsRepository};
use tramontana_persistence::SqliteProviderSettingsRepository;
use tramontana_platform::SystemClock;

use crate::wiring::{self, ProfileArgs};

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
    let outcome = match args.command {
        ProviderCommand::AddOpenai(args) => {
            let new = NewEndpoint::OpenAiCompatible {
                base_url: args.base_url,
                model_id: args.model,
                credential_key: args.credential,
            };
            add(args.profile, &args.id, args.name, new).await
        }
        ProviderCommand::AddAnthropic(args) => {
            let new = NewEndpoint::Anthropic {
                base_url: args.base_url,
                model_id: args.model,
                credential_key: args.credential,
            };
            add(args.profile, &args.id, args.name, new).await
        }
        ProviderCommand::AddLlama(args) => {
            let new = NewEndpoint::LlamaServer {
                binary_path: args.binary,
                model_path: args.model_path,
                model_id: args.model,
            };
            add(args.profile, &args.id, args.name, new).await
        }
        ProviderCommand::AddMlx(args) => {
            let new = NewEndpoint::MlxServer {
                binary_path: args.binary,
                model_path: args.model_path,
                model_id: args.model,
            };
            add(args.profile, &args.id, args.name, new).await
        }
        ProviderCommand::List(args) => list(args).await,
        ProviderCommand::Enable(args) => toggle(args, false).await,
        ProviderCommand::ToolCalls(args) => toggle(args, true).await,
        ProviderCommand::Remove(args) => remove(args).await,
        ProviderCommand::Check(args) => check(args).await,
        ProviderCommand::Trust(args) => trust(args).await,
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Open just the settings repository.
///
/// Not the whole task service: configuring an endpoint must not start a model
/// runtime, and it must work on a profile whose configured runtime is
/// currently broken.
async fn settings(profile: ProfileArgs) -> Result<SqliteProviderSettingsRepository, String> {
    let path = wiring::profile_path(profile.profile);
    let database = tramontana_composition::open_database(&path).await?;
    Ok(SqliteProviderSettingsRepository::new(database))
}

fn message(error: ErrorEnvelope) -> String {
    error.message
}

async fn add(
    profile: ProfileArgs,
    id: &str,
    name: Option<String>,
    new: NewEndpoint,
) -> Result<(), String> {
    // The same clock the durable record uses, so a settings row and an event
    // that mention the same moment agree about what it was.
    let endpoint = endpoints::add(
        &settings(profile).await?,
        id,
        name,
        new,
        SystemClock::new().now(),
    )
    .await
    .map_err(message)?;

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
    Ok(())
}

async fn list(args: ProfileArgs) -> Result<(), String> {
    let endpoints = endpoints::list(&settings(args).await?)
        .await
        .map_err(message)?;
    if endpoints.is_empty() {
        println!("no model endpoints are configured; tasks will use the deterministic fake");
        return Ok(());
    }

    for endpoint in endpoints {
        let files = match tramontana_composition::runtime_files(&endpoint.config) {
            Some((binary, _, recorded)) => {
                let check =
                    tramontana_composition::check_executable(Path::new(binary), recorded).await;
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
    Ok(())
}

async fn toggle(args: ToggleArgs, capability: bool) -> Result<(), String> {
    let wanted = !args.off;
    let change = if capability {
        Toggle::ToolCalls(wanted)
    } else {
        Toggle::Enabled(wanted)
    };
    endpoints::toggle(&settings(args.profile).await?, &args.id, change)
        .await
        .map_err(message)?;
    println!(
        "{}: {} {}",
        args.id,
        if capability {
            "verified tool-call support"
        } else {
            "endpoint"
        },
        if wanted { "enabled" } else { "disabled" }
    );
    Ok(())
}

async fn remove(args: RemoveArgs) -> Result<(), String> {
    let id = args.id;
    let removed = settings(args.profile)
        .await?
        .delete(&id)
        .await
        .map_err(|error| format!("could not remove the endpoint: {error}"))?;
    if removed {
        println!("{id}: removed");
        // Said explicitly. Deleting the credential too would destroy
        // something the OS store may share with other applications, and
        // silently leaving it would be a surprise later.
        println!(
            "  its credential, if any, is still in the OS store; `tramontana secret forget` removes that"
        );
    } else {
        println!("{id}: nothing was configured");
    }
    Ok(())
}

/// How an executable check reads in a listing.
fn describe_executable(check: FileCheck) -> &'static str {
    match check {
        FileCheck::Unchanged => "confirmed",
        FileCheck::Changed => "HELD(executable-changed)",
        FileCheck::Unreadable => "HELD(executable-unreadable)",
        FileCheck::NotRecorded => "not-recorded",
    }
}

async fn check(args: RemoveArgs) -> Result<(), String> {
    let endpoint = endpoints::find(&settings(args.profile).await?, &args.id)
        .await
        .map_err(message)?;
    let (binary, model, recorded) = endpoints::runtime(&endpoint).map_err(message)?;
    let executable = tramontana_composition::check_executable(Path::new(binary), recorded).await;
    let weights = tramontana_composition::check_model(Path::new(model), recorded).await;
    println!("executable: {}", describe_executable(executable));
    println!(
        "model: {}",
        match weights {
            FileCheck::Unchanged => "as recorded",
            FileCheck::Changed => "CHANGED since it was recorded",
            FileCheck::Unreadable => "could not be read",
            FileCheck::NotRecorded => "not recorded",
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
    Ok(())
}

async fn trust(args: RemoveArgs) -> Result<(), String> {
    endpoints::trust(
        &settings(args.profile).await?,
        &args.id,
        SystemClock::new().now(),
    )
    .await
    .map_err(message)?;
    println!(
        "{}: executable and model confirmed as they are now; takes effect the next time Tramontana starts",
        args.id
    );
    Ok(())
}

fn location_label(location: ModelLocation) -> &'static str {
    match location {
        ModelLocation::Local => "local",
        ModelLocation::Cloud => "CLOUD",
    }
}
