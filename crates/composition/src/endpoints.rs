//! Configuring where models can be reached: the rules the CLI's `provider`
//! commands and the desktop's settings view share, so the two cannot accept
//! different endpoints.
//!
//! A credential is never given here, only the key it is filed under in the
//! operating system store (ADR 0008).

use std::path::{Path, PathBuf};

use tramontana_core::contracts::{
    CorrelationId, ErrorCode, ErrorEnvelope, ModelLocation, ProviderConfig, ProviderEndpoint,
    SecretRef, SecretStore, Timestamp,
};
use tramontana_core::ports::ProviderSettingsRepository;

use crate::{record_provenance, runtime_files, with_provenance};

/// Longest endpoint identifier or credential key accepted.
const MAX_NAME_BYTES: usize = 128;

/// An endpoint to configure.
#[derive(Debug, Clone)]
pub enum NewEndpoint {
    /// Anything speaking the OpenAI chat-completions format.
    OpenAiCompatible {
        /// Where it listens.
        base_url: String,
        /// The model to ask for.
        model_id: String,
        /// Key in the OS credential store, if it needs one.
        credential_key: Option<String>,
    },
    /// Anthropic's Messages API, which needs a credential.
    Anthropic {
        /// Where it listens.
        base_url: String,
        /// The model to ask for.
        model_id: String,
        /// Key in the OS credential store.
        credential_key: String,
    },
    /// A `llama-server` this application starts and stops.
    LlamaServer {
        /// The executable. Never discovered from `PATH`.
        binary_path: PathBuf,
        /// The GGUF model file.
        model_path: PathBuf,
        /// The name the model is reported under.
        model_id: String,
    },
    /// An `mlx_lm.server` this application starts and stops (ADR 0010).
    MlxServer {
        /// The executable. Never discovered from `PATH`.
        binary_path: PathBuf,
        /// The model folder: weights, tokenizer and config.
        model_path: PathBuf,
        /// The name the model is reported under.
        model_id: String,
    },
}

/// What to change about an endpoint.
#[derive(Debug, Clone, Copy)]
pub enum Toggle {
    /// Whether the router may consider it.
    Enabled(bool),
    /// Whether tool-call support was verified.
    ToolCalls(bool),
}

/// Configure a new endpoint and return it as saved.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for a malformed name, key or model, plain
/// http to anything off this device (ADR 0009), or a path that is not a file;
/// [`ErrorCode::Conflict`] when an endpoint by that name exists; or
/// [`ErrorCode::PersistenceError`].
pub async fn add(
    settings: &dyn ProviderSettingsRepository,
    endpoint_id: &str,
    display_name: Option<String>,
    new: NewEndpoint,
    now: Timestamp,
) -> Result<ProviderEndpoint, ErrorEnvelope> {
    check_name("an endpoint name", endpoint_id)?;
    let config = match new {
        NewEndpoint::OpenAiCompatible {
            base_url,
            model_id,
            credential_key,
        } => {
            check_model(&model_id)?;
            let credential = credential_key.as_deref().map(credential).transpose()?;
            ProviderConfig::OpenAiCompatible {
                base_url: check_url(&base_url)?,
                model_id,
                credential,
            }
        }
        NewEndpoint::Anthropic {
            base_url,
            model_id,
            credential_key,
        } => {
            check_model(&model_id)?;
            ProviderConfig::Anthropic {
                base_url: check_url(&base_url)?,
                model_id,
                credential: credential(&credential_key)?,
            }
        }
        NewEndpoint::LlamaServer {
            binary_path,
            model_path,
            model_id,
        } => {
            check_model(&model_id)?;
            ProviderConfig::LlamaServer {
                binary_path: check_file("the llama-server executable", &binary_path)?,
                model_path: check_file("the model file", &model_path)?,
                model_id,
                extra_args: Vec::new(),
                provenance: None,
            }
        }
        NewEndpoint::MlxServer {
            binary_path,
            model_path,
            model_id,
        } => {
            check_model(&model_id)?;
            ProviderConfig::MlxServer {
                binary_path: check_file("the mlx_lm.server executable", &binary_path)?,
                model_path: check_folder("the MLX model", &model_path)?,
                model_id,
                extra_args: Vec::new(),
                provenance: None,
            }
        }
    };
    // Plain http carries whatever is sent in the clear. It is accepted only
    // where that never leaves this device (ADR 0009).
    if let ProviderConfig::OpenAiCompatible { base_url, .. }
    | ProviderConfig::Anthropic { base_url, .. } = &config
        && base_url.starts_with("http://")
        && config.location() == ModelLocation::Cloud
    {
        return Err(invalid(
            "plain http is accepted only for an endpoint on this device; use https",
        ));
    }

    // Saving replaces, so a name already in use would silently overwrite
    // someone's configuration.
    if list(settings)
        .await?
        .iter()
        .any(|endpoint| endpoint.endpoint_id == endpoint_id)
    {
        return Err(error(
            ErrorCode::Conflict,
            format!("an endpoint named {endpoint_id} is already configured"),
        ));
    }

    // A runtime Tramontana starts is recorded as it is now, so a later
    // change to its executable is held rather than run (ADR 0043).
    let config = match runtime_files(&config) {
        Some((binary, model, _)) => {
            let recorded = record_provenance(Path::new(binary), Path::new(model), now.clone())
                .await
                .map_err(invalid)?;
            with_provenance(config.clone(), recorded)
        }
        None => config,
    };

    let endpoint = ProviderEndpoint::new(
        endpoint_id,
        display_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| endpoint_id.to_owned()),
        config,
        now,
    );
    save(settings, &endpoint).await?;
    Ok(endpoint)
}

/// Record a runtime's executable and model as they are now, after the person
/// confirmed a change (ADR 0043). Takes effect the next time Tramontana
/// opens.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when no endpoint has that name,
/// [`ErrorCode::InvalidRequest`] when it is not a runtime Tramontana starts
/// or its files cannot be read, or [`ErrorCode::PersistenceError`].
pub async fn trust(
    settings: &dyn ProviderSettingsRepository,
    endpoint_id: &str,
    now: Timestamp,
) -> Result<(), ErrorEnvelope> {
    let mut endpoint = find(settings, endpoint_id).await?;
    let (binary, model, _) = runtime(&endpoint)?;
    let recorded = record_provenance(Path::new(binary), Path::new(model), now)
        .await
        .map_err(invalid)?;
    endpoint.config = with_provenance(endpoint.config.clone(), recorded);
    save(settings, &endpoint).await
}

/// Change one switch on an endpoint, keeping the rest of its configuration.
///
/// # Errors
///
/// As [`find`], or [`ErrorCode::PersistenceError`] when it cannot be saved.
pub async fn toggle(
    settings: &dyn ProviderSettingsRepository,
    endpoint_id: &str,
    toggle: Toggle,
) -> Result<(), ErrorEnvelope> {
    let mut endpoint = find(settings, endpoint_id).await?;
    match toggle {
        Toggle::Enabled(enabled) => endpoint.enabled = enabled,
        Toggle::ToolCalls(supported) => endpoint.supports_tool_calls = supported,
    }
    save(settings, &endpoint).await
}

/// One endpoint by identifier.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when no endpoint has that name, or
/// [`ErrorCode::PersistenceError`] when the settings cannot be read.
pub async fn find(
    settings: &dyn ProviderSettingsRepository,
    endpoint_id: &str,
) -> Result<ProviderEndpoint, ErrorEnvelope> {
    list(settings)
        .await?
        .into_iter()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .ok_or_else(|| {
            error(
                ErrorCode::NotFound,
                format!("no endpoint is named {endpoint_id}"),
            )
        })
}

/// A runtime Tramontana starts: its executable, model and record.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for any other endpoint.
pub fn runtime(
    endpoint: &ProviderEndpoint,
) -> Result<
    (
        &str,
        &str,
        Option<&tramontana_core::contracts::RuntimeProvenance>,
    ),
    ErrorEnvelope,
> {
    runtime_files(&endpoint.config).ok_or_else(|| {
        invalid(format!(
            "{} is not a runtime Tramontana starts",
            endpoint.endpoint_id
        ))
    })
}

/// Every configured endpoint.
///
/// # Errors
///
/// [`ErrorCode::PersistenceError`] when the settings cannot be read.
pub async fn list(
    settings: &dyn ProviderSettingsRepository,
) -> Result<Vec<ProviderEndpoint>, ErrorEnvelope> {
    settings.list().await.map_err(|failure| {
        error(
            ErrorCode::PersistenceError,
            format!("could not read the endpoints: {failure}"),
        )
    })
}

/// An opaque handle to a credential in the platform store.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for a malformed key.
pub fn credential(key: &str) -> Result<SecretRef, ErrorEnvelope> {
    check_name("a credential key", key)?;
    Ok(SecretRef {
        key: key.to_owned(),
        store: SecretStore::Platform,
    })
}

async fn save(
    settings: &dyn ProviderSettingsRepository,
    endpoint: &ProviderEndpoint,
) -> Result<(), ErrorEnvelope> {
    settings.save(endpoint).await.map_err(|failure| {
        error(
            ErrorCode::PersistenceError,
            format!("could not save the endpoint: {failure}"),
        )
    })
}

fn check_name(what: &str, name: &str) -> Result<(), ErrorEnvelope> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(invalid(format!(
            "{what} must be 1 to {MAX_NAME_BYTES} characters, with no spaces"
        )));
    }
    Ok(())
}

fn check_model(model_id: &str) -> Result<(), ErrorEnvelope> {
    if model_id.trim().is_empty() || model_id.len() > MAX_NAME_BYTES {
        return Err(invalid("a model name is needed"));
    }
    Ok(())
}

fn check_url(base_url: &str) -> Result<String, ErrorEnvelope> {
    let base_url = base_url.trim();
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        return Err(invalid("the address must start with https:// or http://"));
    }
    Ok(base_url.to_owned())
}

fn check_file(what: &str, path: &Path) -> Result<String, ErrorEnvelope> {
    if !path.is_file() {
        return Err(invalid(format!("{what} is not a file")));
    }
    Ok(path.display().to_string())
}

/// An MLX model is a folder of weights, tokenizer and config, not one file.
fn check_folder(what: &str, path: &Path) -> Result<String, ErrorEnvelope> {
    if !path.is_dir() {
        return Err(invalid(format!(
            "{what} is not a folder; choose the folder holding its weights and config"
        )));
    }
    Ok(path.display().to_string())
}

fn invalid(message: impl Into<String>) -> ErrorEnvelope {
    error(ErrorCode::InvalidRequest, message)
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("endpoints"))
}
