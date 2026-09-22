//! Configuring Apps: MCP servers Tramontana starts (ADR 0036), or reaches
//! over HTTP (ADR 0050).
//!
//! Apps are profile-scoped, as model endpoints are. A secret is never given
//! here: `--secret NAME=KEY` names a key in the operating system store, put
//! there by `tramontana secret set`, and only that reference reaches the
//! database (ADR 0008).

use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Subcommand};
use tramontana_core::contracts::{
    AppConfig, AppLaunch, AppRemote, AppSecret, AppVariable, RiskClass, SecretRef, SecretStore,
    is_loopback_url, is_valid_app_id,
};
use tramontana_core::ports::{AppSettingsRepository, Clock, SecretResolver, ToolProvider};
use tramontana_persistence::{Database, SqliteAppSettingsRepository};
use tramontana_platform::{PlatformSecretVault, SystemClock};

use crate::wiring;

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<std::path::PathBuf>,
}

/// Arguments for `tramontana app`.
#[derive(Debug, Args)]
pub(crate) struct AppArgs {
    #[command(subcommand)]
    command: AppCommand,
}

#[derive(Debug, Subcommand)]
enum AppCommand {
    /// Configure an MCP server Tramontana starts over stdio.
    Add(AddArgs),
    /// Configure a remote MCP server reached over HTTP (ADR 0050).
    AddRemote(AddRemoteArgs),
    /// List configured Apps.
    List(ProfileOnly),
    /// Turn an App on or off without losing its configuration.
    Enable(ToggleArgs),
    /// Remove an App. A secret it named stays in the OS store.
    Remove(IdArgs),
    /// Start an App, list the tools it offers and those refused, and stop it.
    Tools(IdArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Identifier: lowercase letters, digits, `_` and `-`. Its tools become
    /// `mcp.<ID>.<tool>`.
    #[arg(value_name = "ID")]
    id: String,
    /// The server executable, as a full path. `PATH` is not searched.
    #[arg(long, value_name = "PATH")]
    program: std::path::PathBuf,
    /// An argument for the program. Repeat for more.
    #[arg(long = "arg", value_name = "ARG", allow_hyphen_values = true)]
    arguments: Vec<String>,
    /// Its working folder. Without it, an empty folder of its own beside the
    /// profile.
    #[arg(long, value_name = "PATH")]
    folder: Option<std::path::PathBuf>,
    /// A variable given by value, `NAME=VALUE`. Not for secrets.
    #[arg(long = "env", value_name = "NAME=VALUE")]
    environment: Vec<String>,
    /// A variable given from the OS credential store, `NAME=KEY`.
    #[arg(long = "secret", value_name = "NAME=KEY")]
    secrets: Vec<String>,
    /// Display name. Defaults to the identifier.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct AddRemoteArgs {
    /// Identifier: lowercase letters, digits, `_` and `-`. Its tools become
    /// `mcp.<ID>.<tool>`.
    #[arg(value_name = "ID")]
    id: String,
    /// Its address: `https://`, or `http://` on this computer only.
    #[arg(long, value_name = "URL")]
    url: String,
    /// The key of its token in the OS credential store, put there by
    /// `tramontana secret set`. Sent to this address only, as a bearer token.
    #[arg(long, value_name = "KEY")]
    token_secret: Option<String>,
    /// Display name. Defaults to the identifier.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct ProfileOnly {
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct ToggleArgs {
    /// Which App.
    #[arg(value_name = "ID")]
    id: String,
    /// Turn it off rather than on.
    #[arg(long)]
    off: bool,
    #[command(flatten)]
    profile: ProfileArgs,
}

#[derive(Debug, Args)]
struct IdArgs {
    /// Which App.
    #[arg(value_name = "ID")]
    id: String,
    #[command(flatten)]
    profile: ProfileArgs,
}

/// Run an `app` subcommand.
pub(crate) async fn execute(args: AppArgs) -> ExitCode {
    let result = match args.command {
        AppCommand::Add(args) => add(args).await,
        AppCommand::AddRemote(args) => add_remote(args).await,
        AppCommand::List(args) => list(args.profile).await,
        AppCommand::Enable(args) => toggle(args).await,
        AppCommand::Remove(args) => remove(args).await,
        AppCommand::Tools(args) => tools(args).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Open just the App settings. Configuring an App must not start one.
async fn settings(profile: ProfileArgs) -> Result<SqliteAppSettingsRepository, String> {
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
    Ok(SqliteAppSettingsRepository::new(database))
}

fn pair(text: &str, what: &str) -> Result<(String, String), String> {
    let (name, value) = text
        .split_once('=')
        .ok_or_else(|| format!("{what} `{text}` is not NAME=VALUE"))?;
    if name.is_empty() || name.contains('\0') {
        return Err(format!("{what} `{text}` has no usable name"));
    }
    Ok((name.to_owned(), value.to_owned()))
}

async fn add(args: AddArgs) -> Result<(), String> {
    if !is_valid_app_id(&args.id) {
        return Err(format!(
            "`{}` is not a valid App identifier: lowercase letters, digits, `_` and `-`",
            args.id
        ));
    }
    if !args.program.is_absolute() {
        return Err("--program must be a full path; PATH is not searched".to_owned());
    }
    let environment = args
        .environment
        .iter()
        .map(|text| pair(text, "--env").map(|(name, value)| AppVariable { name, value }))
        .collect::<Result<Vec<_>, _>>()?;
    let secrets = args
        .secrets
        .iter()
        .map(|text| {
            pair(text, "--secret").map(|(name, key)| AppSecret {
                name,
                reference: SecretRef {
                    key,
                    store: SecretStore::Platform,
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let settings = settings(args.profile).await?;
    let config = AppConfig::new(
        &args.id,
        args.name.clone().unwrap_or_else(|| args.id.clone()),
        AppLaunch {
            program: args.program.display().to_string(),
            arguments: args.arguments.clone(),
            working_directory: args.folder.as_ref().map(|path| path.display().to_string()),
            environment,
            secrets,
        },
        SystemClock::new().now(),
    );
    settings
        .save(&config)
        .await
        .map_err(|error| format!("could not save the App: {error}"))?;

    println!(
        "configured {} -> {} (its tools become mcp.{}.<tool>)",
        config.app_id, config.launch.program, config.app_id
    );
    match &config.launch.working_directory {
        Some(folder) => println!("  runs in {folder}"),
        None => println!("  runs in an empty folder of its own beside the profile"),
    }
    for secret in &config.launch.secrets {
        println!(
            "  {} from the credential store (set it with `tramontana secret set {}`)",
            secret.name, secret.reference.key
        );
    }
    println!("  a task is offered its tools only with --apps, and each call is asked about");
    Ok(())
}

async fn add_remote(args: AddRemoteArgs) -> Result<(), String> {
    if !is_valid_app_id(&args.id) {
        return Err(format!(
            "`{}` is not a valid App identifier: lowercase letters, digits, `_` and `-`",
            args.id
        ));
    }
    let local = is_loopback_url(&args.url);
    if !(args.url.starts_with("https://") || args.url.starts_with("http://") && local) {
        return Err("--url must be https://, or http:// on this computer".to_owned());
    }
    let settings = settings(args.profile).await?;
    let config = AppConfig::remote(
        &args.id,
        args.name.clone().unwrap_or_else(|| args.id.clone()),
        AppRemote {
            url: args.url.clone(),
            token: args.token_secret.clone().map(|key| SecretRef {
                key,
                store: SecretStore::Platform,
            }),
        },
        SystemClock::new().now(),
    );
    settings
        .save(&config)
        .await
        .map_err(|error| format!("could not save the App: {error}"))?;
    println!(
        "configured {} -> {} (its tools become mcp.{}.<tool>)",
        config.app_id, args.url, config.app_id
    );
    if let Some(key) = &args.token_secret {
        println!(
            "  its token from the credential store (set it with `tramontana secret set {key}`)"
        );
    }
    if local {
        println!("  on this computer: offered under Local Only too");
    } else {
        println!(
            "  off this computer: never offered under Local Only, and every call says where it goes"
        );
    }
    println!("  a task is offered its tools only with --apps, and each call is asked about");
    Ok(())
}

async fn list(profile: ProfileArgs) -> Result<(), String> {
    let apps = settings(profile)
        .await?
        .list()
        .await
        .map_err(|error| format!("could not read Apps: {error}"))?;
    if apps.is_empty() {
        println!("no Apps are configured");
        return Ok(());
    }
    for app in apps {
        println!(
            "{:<20} {:<8} {}",
            app.app_id,
            if app.enabled { "enabled" } else { "disabled" },
            app.remote
                .as_ref()
                .map_or(app.launch.program.as_str(), |remote| remote.url.as_str()),
        );
    }
    Ok(())
}

async fn find(settings: &SqliteAppSettingsRepository, id: &str) -> Result<AppConfig, String> {
    settings
        .list()
        .await
        .map_err(|error| format!("could not read Apps: {error}"))?
        .into_iter()
        .find(|app| app.app_id == id)
        .ok_or_else(|| format!("no App is configured as `{id}`"))
}

async fn toggle(args: ToggleArgs) -> Result<(), String> {
    let settings = settings(args.profile).await?;
    let mut app = find(&settings, &args.id).await?;
    app.enabled = !args.off;
    settings
        .save(&app)
        .await
        .map_err(|error| format!("could not save the App: {error}"))?;
    println!(
        "{} is {}",
        app.app_id,
        if app.enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

async fn remove(args: IdArgs) -> Result<(), String> {
    let removed = settings(args.profile)
        .await?
        .delete(&args.id)
        .await
        .map_err(|error| format!("could not remove the App: {error}"))?;
    if !removed {
        return Err(format!("no App is configured as `{}`", args.id));
    }
    println!("removed {}", args.id);
    Ok(())
}

async fn tools(args: IdArgs) -> Result<(), String> {
    let path = wiring::profile_path(args.profile.profile.clone());
    let settings = settings(args.profile).await?;
    let config = find(&settings, &args.id).await?;
    let resolver = Arc::new(PlatformSecretVault::new()) as Arc<dyn SecretResolver>;
    let app = tramontana_composition::start_app(&config, &resolver, &path).await?;

    println!(
        "{} answered on MCP {}",
        config.app_id,
        app.protocol_version()
    );
    let offered = app.list_tools();
    if offered.is_empty() {
        println!("  it offers no tool Tramontana will register");
    }
    for tool in offered {
        println!(
            "  {:<32} {}",
            tool.tool_id,
            if tool.risk_class == RiskClass::Read {
                "says it only reads; a task with --apps may be offered it"
            } else {
                "says nothing about what it does; not offered to a model"
            }
        );
    }
    for refusal in app.refused() {
        println!("  refused {}: {}", refusal.name, refusal.reason);
    }
    app.shutdown().await;
    Ok(())
}
