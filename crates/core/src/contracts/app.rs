//! How a person configured an App: an MCP server Tramontana starts on their
//! behalf (ADR 0036).
//!
//! As for model endpoints, **a secret is never here**. A variable an App
//! needs from the credential store is an [`AppSecret`]: a name and a
//! [`SecretRef`], resolved only as the App starts. A variable given by value
//! is an [`AppVariable`], for settings that are not secret.
//!
//! App configuration belongs to the profile, not to a project. It is not
//! exported with a project, and importing a bundle never starts an App
//! (`PROJECT_FORMAT.md` §5).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{SCHEMA_VERSION, SecretRef, Timestamp};

/// The longest App identifier.
pub const MAX_APP_ID: usize = 32;

/// Whether `app_id` can identify an App.
///
/// Lowercase letters, digits, `_` and `-`, starting with a letter or digit,
/// at most [`MAX_APP_ID`]. It namespaces the App's tools as `mcp.<app_id>.`
/// and names its folder beside the profile, so nothing that could leave
/// either is accepted.
#[must_use]
pub fn is_valid_app_id(app_id: &str) -> bool {
    let starts_well = app_id
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    starts_well
        && app_id.len() <= MAX_APP_ID
        && app_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// An environment variable an App is given by value. Not for secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppVariable {
    /// The variable's name.
    pub name: String,
    /// Its value, stored in the profile as written.
    pub value: String,
}

/// An environment variable an App is given from the credential store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppSecret {
    /// The variable's name.
    pub name: String,
    /// Where its value is kept. Resolved only as the App starts.
    pub reference: SecretRef,
}

/// How an App is started over stdio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppLaunch {
    /// The program's full path. `PATH` is not searched.
    pub program: String,
    /// Its arguments.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Its working folder. Absent means an empty folder of its own beside
    /// the profile, never a project's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Variables it is given by value.
    #[serde(default)]
    pub environment: Vec<AppVariable>,
    /// Variables it is given from the credential store.
    #[serde(default)]
    pub secrets: Vec<AppSecret>,
}

/// Where a remote App is, and how Tramontana signs in to it (ADR 0050).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppRemote {
    /// Its address: `https://`, or `http://` on this computer only.
    pub url: String,
    /// The token sent to it as a bearer credential, by reference to the
    /// credential store. Absent for an App that needs none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<SecretRef>,
    /// The client identifier this application signs in with, when the App
    /// asked for OAuth: issued by the authorization server when it allows
    /// registration, or given by the person (ADR 0056). Not a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Where the refresh token lives, when signing in produced one. Like the
    /// token itself, by reference: the value stays in the credential store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<SecretRef>,
}

/// One configured App.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    /// Contract schema version.
    pub schema_version: u32,
    /// Its identifier; see [`is_valid_app_id`].
    pub app_id: String,
    /// What to call it.
    pub display_name: String,
    /// Whether it is started with the application.
    ///
    /// Separate from removing it, as for a model endpoint: turning an App off
    /// for a while should not cost its configuration.
    pub enabled: bool,
    /// How to start it, for an App that runs on this computer over stdio.
    /// Empty for a remote one.
    pub launch: AppLaunch,
    /// Where it is, for an App reached over HTTP instead (ADR 0050).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<AppRemote>,
    /// When it was configured.
    pub created_at: Timestamp,
}

impl AppConfig {
    /// Configure an App, enabled.
    #[must_use]
    pub fn new(
        app_id: impl Into<String>,
        display_name: impl Into<String>,
        launch: AppLaunch,
        created_at: Timestamp,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            app_id: app_id.into(),
            display_name: display_name.into(),
            enabled: true,
            launch,
            remote: None,
            created_at,
        }
    }

    /// Configure a remote App, enabled (ADR 0050).
    #[must_use]
    pub fn remote(
        app_id: impl Into<String>,
        display_name: impl Into<String>,
        remote: AppRemote,
        created_at: Timestamp,
    ) -> Self {
        Self {
            remote: Some(remote),
            ..Self::new(
                app_id,
                display_name,
                AppLaunch {
                    program: String::new(),
                    arguments: Vec::new(),
                    working_directory: None,
                    environment: Vec::new(),
                    secrets: Vec::new(),
                },
                created_at,
            )
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn an_app_id_that_could_leave_its_namespace_or_folder_is_not_valid() {
        for good in ["notes", "a", "my-app_2"] {
            assert!(is_valid_app_id(good), "{good}");
        }
        for bad in ["", "Notes", "../x", "a.b", "a/b", "-app", &"n".repeat(33)] {
            assert!(!is_valid_app_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_secret_is_carried_by_reference_only() {
        let launch = AppLaunch {
            program: "/opt/app/server".to_owned(),
            arguments: Vec::new(),
            working_directory: None,
            environment: vec![AppVariable {
                name: "LOG_LEVEL".to_owned(),
                value: "info".to_owned(),
            }],
            secrets: vec![AppSecret {
                name: "API_TOKEN".to_owned(),
                reference: SecretRef {
                    key: "notes.token".to_owned(),
                    store: super::super::SecretStore::Platform,
                },
            }],
        };
        let config = AppConfig::new(
            "notes",
            "Notes",
            launch,
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let json = serde_json::to_string(&config).expect("serializes");
        assert!(json.contains("\"key\":\"notes.token\""), "{json}");
        assert_eq!(
            serde_json::from_str::<AppConfig>(&json).expect("reads back"),
            config
        );
    }
}
