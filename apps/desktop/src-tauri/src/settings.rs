//! Model endpoints and credentials, configured from the window.
//!
//! The same settings the CLI's `provider` and `secret` commands change, in the
//! same profile and the same operating-system store. The commands ask the
//! native picker for any path; the work is here, where it can be tested
//! without a window and without touching a real credential store.
//!
//! An endpoint saved here takes effect the next time Tramontana opens. The
//! running service was assembled with the models configured when it started,
//! and swapping them under a running task would change what that task's
//! routing decisions meant.

use crate::ipc_error;
use std::path::Path;

use tramontana_composition::endpoints::{self, credential};
pub(crate) use tramontana_composition::endpoints::{NewEndpoint, Toggle, toggle, trust};
use tramontana_core::contracts::{ErrorCode, ErrorEnvelope, ModelLocation, Timestamp};
use tramontana_core::ports::{ProviderSettingsRepository, SecretVault};

/// Longest credential value accepted from the window.
const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;

/// Whether a credential is stored, without its value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CredentialStatus {
    /// The key it is filed under, such as `openai.apiKey`. Never the value.
    pub key: String,
    /// Whether one is stored; absent when the store could not answer.
    ///
    /// A locked store and an empty one are told apart: a person whose store
    /// is locked must not be sent to enter a key that is already there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
    /// Why the store could not answer, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// One configured endpoint, as the settings view shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfiguredEndpoint {
    pub endpoint_id: String,
    pub display_name: String,
    /// `openAiCompatible`, `anthropic` or `llamaServer`.
    pub kind: &'static str,
    pub model_id: String,
    /// Whether work sent here leaves the device. Derived, never declared.
    pub leaves_the_device: bool,
    pub enabled: bool,
    /// Verified tool-call support, recorded by the person. Unknown is off.
    pub supports_tool_calls: bool,
    /// The credential it uses, when it uses one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialStatus>,
    /// A runtime Tramontana starts: its files and whether they are still
    /// what was recorded (ADR 0043).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeFiles>,
}

/// A supervised runtime's files, as the settings view shows them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeFiles {
    /// The executable's file name. Its full path names a folder, and usually
    /// a person.
    pub executable: String,
    /// `confirmed`, `held` (changed since recorded), `unreadable`, or
    /// `notRecorded` for an endpoint added before records were kept.
    pub executable_status: &'static str,
    /// The model's size in bytes, as recorded: the least memory it needs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_bytes: Option<u64>,
    /// When the files were recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
}

/// How a file check reads in the settings view.
pub(crate) fn describe_check(check: tramontana_composition::FileCheck) -> &'static str {
    match check {
        tramontana_composition::FileCheck::Unchanged => "confirmed",
        tramontana_composition::FileCheck::Changed => "held",
        tramontana_composition::FileCheck::Unreadable => "unreadable",
        tramontana_composition::FileCheck::NotRecorded => "notRecorded",
    }
}

/// Every configured endpoint, with whether its credential is stored.
///
/// # Errors
///
/// [`ErrorCode::PersistenceError`] when the settings cannot be read.
pub(crate) async fn list(
    settings: &dyn ProviderSettingsRepository,
    vault: &dyn SecretVault,
) -> Result<Vec<ConfiguredEndpoint>, ErrorEnvelope> {
    let endpoints = endpoints::list(settings).await?;
    let mut configured = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        let credential = match endpoint.config.credential() {
            None => None,
            Some(reference) => Some(match vault.contains(reference).await {
                Ok(stored) => CredentialStatus {
                    key: reference.key.clone(),
                    configured: Some(stored),
                    unavailable_reason: None,
                },
                Err(error) => CredentialStatus {
                    key: reference.key.clone(),
                    configured: None,
                    unavailable_reason: Some(error.message),
                },
            }),
        };
        let runtime = match tramontana_composition::runtime_files(&endpoint.config) {
            None => None,
            Some((binary, _, recorded)) => {
                let path = Path::new(binary);
                let check = tramontana_composition::check_executable(path, recorded).await;
                Some(RuntimeFiles {
                    executable: path.file_name().map_or_else(
                        || binary.to_owned(),
                        |name| name.to_string_lossy().into_owned(),
                    ),
                    executable_status: describe_check(check),
                    model_bytes: recorded.map(|recorded| recorded.model_bytes),
                    recorded_at: recorded.map(|recorded| recorded.recorded_at.as_str().to_owned()),
                })
            }
        };
        configured.push(ConfiguredEndpoint {
            runtime,
            endpoint_id: endpoint.endpoint_id.clone(),
            display_name: endpoint.display_name.clone(),
            kind: endpoint.config.kind(),
            model_id: endpoint.config.model_id().to_owned(),
            leaves_the_device: endpoint.location() == ModelLocation::Cloud,
            enabled: endpoint.enabled,
            supports_tool_calls: endpoint.supports_tool_calls,
            credential,
        });
    }
    Ok(configured)
}

/// Configure a new endpoint. Returns whether work sent to it leaves the device.
///
/// # Errors
///
/// As [`endpoints::add`].
pub(crate) async fn add(
    settings: &dyn ProviderSettingsRepository,
    endpoint_id: &str,
    display_name: Option<String>,
    new: NewEndpoint,
    now: Timestamp,
) -> Result<bool, ErrorEnvelope> {
    endpoints::add(settings, endpoint_id, display_name, new, now)
        .await
        .map(|endpoint| endpoint.location() == ModelLocation::Cloud)
}

/// Whether a runtime's model is still what was recorded. Reads all of it.
///
/// # Errors
///
/// As [`endpoints::trust`], less the persistence error.
pub(crate) async fn check_runtime_model(
    settings: &dyn ProviderSettingsRepository,
    endpoint_id: &str,
) -> Result<&'static str, ErrorEnvelope> {
    let endpoint = endpoints::find(settings, endpoint_id).await?;
    let (_, model, recorded) = endpoints::runtime(&endpoint)?;
    Ok(describe_check(
        tramontana_composition::check_model(Path::new(model), recorded).await,
    ))
}

/// Remove an endpoint. Its credential, if any, stays in the store: deleting
/// it too would destroy something the store may share with other programs.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when no endpoint has that name, or
/// [`ErrorCode::PersistenceError`].
pub(crate) async fn remove(
    settings: &dyn ProviderSettingsRepository,
    endpoint_id: &str,
) -> Result<(), ErrorEnvelope> {
    let removed = settings.delete(endpoint_id).await.map_err(|error| {
        ipc_error(
            ErrorCode::PersistenceError,
            format!("could not remove the endpoint: {error}"),
        )
    })?;
    if removed {
        Ok(())
    } else {
        Err(ipc_error(
            ErrorCode::NotFound,
            format!("no endpoint is named {endpoint_id}"),
        ))
    }
}

/// Store a credential, replacing any value already filed under `key`.
///
/// Returns the number of characters stored, never the value: enough to notice
/// a truncated paste.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for a malformed key or an empty or oversized
/// value, or the store's own normalized error, which never echoes the value.
pub(crate) async fn store_credential(
    vault: &dyn SecretVault,
    key: &str,
    value: &str,
) -> Result<usize, ErrorEnvelope> {
    let reference = credential(key)?;
    // A trailing newline from a paste is almost never part of the credential,
    // and a key with an invisible newline on the end fails for a reason
    // nobody can see.
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        return Err(invalid(
            "nothing was entered; the credential was not stored",
        ));
    }
    if value.len() > MAX_CREDENTIAL_BYTES {
        return Err(invalid("that is too long to be a credential"));
    }
    vault.store(&reference, value).await?;
    Ok(value.chars().count())
}

/// Remove a credential from the store. Returns whether one was there.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for a malformed key, or the store's error.
pub(crate) async fn forget_credential(
    vault: &dyn SecretVault,
    key: &str,
) -> Result<bool, ErrorEnvelope> {
    vault.delete(&credential(key)?).await
}

fn invalid(message: impl Into<String>) -> ErrorEnvelope {
    ipc_error(ErrorCode::InvalidRequest, message)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on isolated fixtures; a panic is the failure report"
)]
mod tests {
    use tramontana_core::testing::InMemorySecretVault;
    use tramontana_persistence::{Database, SqliteProviderSettingsRepository};

    use super::*;

    async fn settings(directory: &tempfile::TempDir) -> SqliteProviderSettingsRepository {
        SqliteProviderSettingsRepository::new(
            Database::open(&directory.path().join("settings.sqlite"))
                .await
                .unwrap(),
        )
    }

    fn now() -> Timestamp {
        Timestamp::new("2026-09-15T00:00:00.000Z")
    }

    fn local(model_id: &str) -> NewEndpoint {
        NewEndpoint::OpenAiCompatible {
            base_url: "http://127.0.0.1:8080".to_owned(),
            model_id: model_id.to_owned(),
            credential_key: None,
        }
    }

    #[tokio::test]
    async fn an_endpoint_added_here_is_listed_as_the_cli_would_see_it() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        let vault = InMemorySecretVault::new();

        let leaves = add(&settings, "local", None, local("qwen"), now())
            .await
            .unwrap();
        assert!(!leaves);

        assert_eq!(
            list(&settings, &vault).await.unwrap(),
            [ConfiguredEndpoint {
                endpoint_id: "local".to_owned(),
                display_name: "local".to_owned(),
                kind: "openAiCompatible",
                model_id: "qwen".to_owned(),
                leaves_the_device: false,
                enabled: true,
                supports_tool_calls: false,
                credential: None,
                runtime: None,
            }]
        );
    }

    #[tokio::test]
    async fn plain_http_off_the_device_a_taken_name_and_a_bad_path_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        add(&settings, "local", None, local("qwen"), now())
            .await
            .unwrap();

        let refusals = [
            (
                "remote",
                NewEndpoint::OpenAiCompatible {
                    base_url: "http://models.example.com".to_owned(),
                    model_id: "m".to_owned(),
                    credential_key: None,
                },
                ErrorCode::InvalidRequest,
            ),
            (
                "claude",
                NewEndpoint::Anthropic {
                    base_url: "http://api.anthropic.com".to_owned(),
                    model_id: "m".to_owned(),
                    credential_key: "anthropic.apiKey".to_owned(),
                },
                ErrorCode::InvalidRequest,
            ),
            ("local", local("other"), ErrorCode::Conflict),
            ("has space", local("qwen"), ErrorCode::InvalidRequest),
            (
                "llama",
                NewEndpoint::LlamaServer {
                    binary_path: directory.path().join("no-such-binary"),
                    model_path: directory.path().join("no-such-model.gguf"),
                    model_id: "qwen".to_owned(),
                },
                ErrorCode::InvalidRequest,
            ),
        ];
        for (name, new, code) in refusals {
            let refused = add(&settings, name, None, new, now()).await.unwrap_err();
            assert_eq!(refused.code, code, "{name}");
        }
        assert_eq!(
            list(&settings, &InMemorySecretVault::new())
                .await
                .unwrap()
                .len(),
            1,
            "a refused endpoint was saved"
        );
    }

    #[tokio::test]
    async fn a_managed_runtime_is_configured_from_picked_files_and_runs_here() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        let binary = directory.path().join("llama-server");
        let model = directory.path().join("model.gguf");
        std::fs::write(&binary, b"").unwrap();
        std::fs::write(&model, b"").unwrap();

        let leaves = add(
            &settings,
            "llama",
            Some("Local Qwen".to_owned()),
            NewEndpoint::LlamaServer {
                binary_path: binary,
                model_path: model,
                model_id: "qwen".to_owned(),
            },
            now(),
        )
        .await
        .unwrap();
        assert!(!leaves);
        let listed = list(&settings, &InMemorySecretVault::new()).await.unwrap();
        assert_eq!(listed[0].kind, "llamaServer");
        assert_eq!(listed[0].display_name, "Local Qwen");
    }

    #[tokio::test]
    async fn an_mlx_runtime_needs_a_model_folder_and_runs_here() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        let binary = directory.path().join("mlx_lm.server");
        let folder = directory.path().join("Qwen-MLX");
        let file = directory.path().join("model.safetensors");
        std::fs::write(&binary, b"").unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(&file, b"").unwrap();

        // One weights file is not a model: MLX needs the folder around it.
        let refused = add(
            &settings,
            "mlx",
            None,
            NewEndpoint::MlxServer {
                binary_path: binary.clone(),
                model_path: file,
                model_id: "qwen".to_owned(),
            },
            now(),
        )
        .await
        .unwrap_err();
        assert!(refused.message.contains("is not a folder"), "{refused:?}");

        let leaves = add(
            &settings,
            "mlx",
            Some("Local Qwen (MLX)".to_owned()),
            NewEndpoint::MlxServer {
                binary_path: binary,
                model_path: folder,
                model_id: "qwen".to_owned(),
            },
            now(),
        )
        .await
        .unwrap();
        assert!(!leaves, "a runtime Tramontana starts is on this device");
        let listed = list(&settings, &InMemorySecretVault::new()).await.unwrap();
        assert_eq!(listed[0].kind, "mlxServer");
        assert!(!listed[0].leaves_the_device);
    }

    #[tokio::test]
    async fn a_runtimes_files_are_recorded_held_when_changed_and_confirmed() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        let binary = directory.path().join("llama-server");
        let model = directory.path().join("model.gguf");
        std::fs::write(&binary, b"version one").unwrap();
        std::fs::write(&model, b"weights").unwrap();
        add(
            &settings,
            "llama",
            None,
            NewEndpoint::LlamaServer {
                binary_path: binary.clone(),
                model_path: model.clone(),
                model_id: "qwen".to_owned(),
            },
            now(),
        )
        .await
        .unwrap();
        let vault = InMemorySecretVault::new();
        let runtime = |listed: Vec<ConfiguredEndpoint>| listed[0].runtime.clone().unwrap();

        let files = runtime(list(&settings, &vault).await.unwrap());
        assert_eq!(files.executable, "llama-server");
        assert_eq!(files.executable_status, "confirmed");
        assert_eq!(files.model_bytes, Some(7));

        // An upgrade: held, and a changed model is found when asked.
        std::fs::write(&binary, b"version two").unwrap();
        std::fs::write(&model, b"new weights").unwrap();
        assert_eq!(
            runtime(list(&settings, &vault).await.unwrap()).executable_status,
            "held"
        );
        assert_eq!(
            check_runtime_model(&settings, "llama").await.unwrap(),
            "held"
        );

        trust(&settings, "llama", now()).await.unwrap();
        let files = runtime(list(&settings, &vault).await.unwrap());
        assert_eq!(files.executable_status, "confirmed");
        assert_eq!(files.model_bytes, Some(11));
        assert_eq!(
            check_runtime_model(&settings, "llama").await.unwrap(),
            "confirmed"
        );
        // Only a runtime has files to confirm.
        add(&settings, "local", None, local("qwen"), now())
            .await
            .unwrap();
        assert!(trust(&settings, "local", now()).await.is_err());
        assert!(trust(&settings, "absent", now()).await.is_err());
    }

    #[tokio::test]
    async fn a_credential_is_reported_as_stored_and_never_read_back() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        let vault = InMemorySecretVault::new();
        let leaves = add(
            &settings,
            "claude",
            None,
            NewEndpoint::Anthropic {
                base_url: "https://api.anthropic.com".to_owned(),
                model_id: "claude".to_owned(),
                credential_key: "anthropic.apiKey".to_owned(),
            },
            now(),
        )
        .await
        .unwrap();
        assert!(leaves, "a third party is not this device");

        let status = |listed: Vec<ConfiguredEndpoint>| listed[0].credential.clone().unwrap();
        assert_eq!(
            status(list(&settings, &vault).await.unwrap()).configured,
            Some(false)
        );

        // A pasted newline is not part of the key; only its length comes back.
        assert_eq!(
            store_credential(&vault, "anthropic.apiKey", "sk-test\n")
                .await
                .unwrap(),
            7
        );
        assert_eq!(
            status(list(&settings, &vault).await.unwrap()).configured,
            Some(true)
        );
        let empty = store_credential(&vault, "anthropic.apiKey", "\n")
            .await
            .unwrap_err();
        assert_eq!(empty.code, ErrorCode::InvalidRequest);

        assert!(forget_credential(&vault, "anthropic.apiKey").await.unwrap());
        assert_eq!(
            status(list(&settings, &vault).await.unwrap()).configured,
            Some(false)
        );
    }

    #[tokio::test]
    async fn a_locked_store_is_not_reported_as_an_empty_one() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        add(
            &settings,
            "openai",
            None,
            NewEndpoint::OpenAiCompatible {
                base_url: "https://api.openai.com".to_owned(),
                model_id: "gpt".to_owned(),
                credential_key: Some("openai.apiKey".to_owned()),
            },
            now(),
        )
        .await
        .unwrap();

        let listed = list(&settings, &InMemorySecretVault::unavailable())
            .await
            .unwrap();
        let credential = listed[0].credential.clone().unwrap();
        assert_eq!(credential.configured, None);
        assert!(credential.unavailable_reason.is_some());
    }

    #[tokio::test]
    async fn switches_and_removal_change_only_what_was_asked() {
        let directory = tempfile::tempdir().unwrap();
        let settings = settings(&directory).await;
        let vault = InMemorySecretVault::new();
        add(&settings, "local", None, local("qwen"), now())
            .await
            .unwrap();

        toggle(&settings, "local", Toggle::Enabled(false))
            .await
            .unwrap();
        toggle(&settings, "local", Toggle::ToolCalls(true))
            .await
            .unwrap();
        let listed = list(&settings, &vault).await.unwrap();
        assert!(!listed[0].enabled);
        assert!(listed[0].supports_tool_calls);
        assert_eq!(listed[0].model_id, "qwen");

        let missing = toggle(&settings, "nobody", Toggle::Enabled(true))
            .await
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);

        remove(&settings, "local").await.unwrap();
        assert!(list(&settings, &vault).await.unwrap().is_empty());
        assert_eq!(
            remove(&settings, "local").await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }
}
