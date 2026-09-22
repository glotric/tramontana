//! Turning configured Apps into running ones (ADR 0036).
//!
//! As for model endpoints, **a broken App does not break the application**:
//! each enabled App is started on its own, and one that fails becomes an
//! [`AppFailure`] while the rest still run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tramontana_adapters_mcp::{McpApp, McpLimits, McpServerSpec};
use tramontana_core::contracts::{AppConfig, is_valid_app_id};
use tramontana_core::ports::{SecretResolver, ToolProvider};

/// Where an App runs when no folder was configured: an empty folder of its
/// own beside the profile, `<profile>.apps/<app_id>`, never a project's.
///
/// ```
/// let folder = tramontana_composition::app_folder(std::path::Path::new("data/p.sqlite"), "notes");
/// assert_eq!(folder, std::path::Path::new("data/p.sqlite.apps/notes"));
/// ```
#[must_use]
pub fn app_folder(profile: &Path, app_id: &str) -> PathBuf {
    let mut name = profile
        .file_name()
        .map_or_else(|| std::ffi::OsString::from("profile"), ToOwned::to_owned);
    name.push(".apps");
    profile.with_file_name(name).join(app_id)
}

/// An App that could not be started, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppFailure {
    /// Which App.
    pub app_id: String,
    /// What to call it.
    pub display_name: String,
    /// Why, in words, with no secret in them.
    pub reason: String,
}

/// Every App that started, and every one that did not.
#[derive(Debug, Default)]
pub struct AppPlane {
    /// The running Apps.
    pub apps: Vec<Arc<McpApp>>,
    /// The Apps that did not start.
    pub failures: Vec<AppFailure>,
}

impl AppPlane {
    /// The running Apps, as tool providers for the registry.
    #[must_use]
    pub fn providers(&self) -> Vec<Arc<dyn ToolProvider>> {
        self.apps
            .iter()
            .map(|app| Arc::clone(app) as Arc<dyn ToolProvider>)
            .collect()
    }

    /// The tools a task that admits Apps may offer its model: only those an
    /// App says only read (ADR 0036).
    ///
    /// A tool that claims nothing is registered, and asked about if anything
    /// calls it, but is not offered: the catalogue refuses a whole selection
    /// that names a tool it cannot admit, so listing one here would fail the
    /// task rather than leave the tool out.
    #[must_use]
    pub fn tool_ids(&self) -> Vec<String> {
        self.apps
            .iter()
            .flat_map(|app| app.list_tools())
            .filter(offerable)
            .map(|tool| tool.tool_id)
            .collect()
    }

    /// Stop every running App.
    pub async fn shutdown(&self) {
        for app in &self.apps {
            app.shutdown().await;
        }
    }
}

/// Whether a task that admits Apps may offer this App tool to its model.
///
/// Only a tool its App says reads and changes nothing. That is the App's claim,
/// and it is not trusted for anything else — the call is still asked about
/// (ADR 0035) — but a tool that does not even claim it cannot be offered.
#[must_use]
pub fn offerable(tool: &tramontana_core::contracts::ToolDescriptor) -> bool {
    tool.risk_class == tramontana_core::contracts::RiskClass::Read
        && tool.side_effect == tramontana_core::contracts::SideEffect::None
}

/// Start each enabled App in `configured`.
///
/// Never fails as a whole: an App that cannot start is an [`AppFailure`].
pub async fn start_apps(
    configured: &[AppConfig],
    resolver: &Arc<dyn SecretResolver>,
    profile: &Path,
) -> AppPlane {
    let mut plane = AppPlane::default();
    for config in configured.iter().filter(|config| config.enabled) {
        match start_app(config, resolver, profile).await {
            Ok(app) => plane.apps.push(Arc::new(app)),
            Err(reason) => plane.failures.push(AppFailure {
                app_id: config.app_id.clone(),
                display_name: config.display_name.clone(),
                reason,
            }),
        }
    }
    plane
}

/// Start one App, whether or not it is enabled.
///
/// # Errors
///
/// Why it could not start, in words with no secret in them.
pub async fn start_app(
    config: &AppConfig,
    resolver: &Arc<dyn SecretResolver>,
    profile: &Path,
) -> Result<McpApp, String> {
    // Checked before its folder is made: the identifier names that folder.
    if !is_valid_app_id(&config.app_id) {
        return Err(format!("`{}` is not a valid App identifier", config.app_id));
    }
    // A remote App is reached, not started: no folder, no process
    // (ADR 0050). Its token is read now and sent only to its address.
    if let Some(remote) = &config.remote {
        let token = match &remote.token {
            Some(reference) => Some(
                resolver
                    .resolve(reference)
                    .await
                    .map_err(|error| format!("its token could not be read: {}", error.message))?,
            ),
            None => None,
        };
        return McpApp::connect(tramontana_adapters_mcp::McpRemoteSpec {
            app_id: config.app_id.clone(),
            display_name: config.display_name.clone(),
            url: remote.url.clone(),
            token,
            limits: McpLimits::default(),
        })
        .await
        .map_err(|error| error.to_string());
    }
    let folder = match &config.launch.working_directory {
        Some(path) => PathBuf::from(path),
        None => {
            let folder = app_folder(profile, &config.app_id);
            std::fs::create_dir_all(&folder)
                .map_err(|error| format!("its folder could not be made: {error}"))?;
            folder
        }
    };

    let mut environment: Vec<(String, String)> = config
        .launch
        .environment
        .iter()
        .map(|variable| (variable.name.clone(), variable.value.clone()))
        .collect();
    for secret in &config.launch.secrets {
        let value = resolver.resolve(&secret.reference).await.map_err(|error| {
            format!(
                "the secret for {} could not be read: {}",
                secret.name, error.message
            )
        })?;
        environment.push((secret.name.clone(), value));
    }

    McpApp::start(McpServerSpec {
        app_id: config.app_id.clone(),
        display_name: config.display_name.clone(),
        program: PathBuf::from(&config.launch.program),
        arguments: config.launch.arguments.clone(),
        working_directory: folder,
        environment,
        limits: McpLimits::default(),
    })
    .await
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use tramontana_core::contracts::{ReplaySafety, RiskClass, SideEffect, ToolDescriptor};

    use super::offerable;

    fn tool(risk_class: RiskClass, side_effect: SideEffect) -> ToolDescriptor {
        ToolDescriptor {
            tool_id: "mcp.fixture.tool".to_owned(),
            provider_id: "mcp.fixture".to_owned(),
            display_name: "tool".to_owned(),
            description: "A tool.".to_owned(),
            input_schema: serde_json::json!({ "type": "object" }),
            risk_class,
            side_effect,
            capability: "app.fixture".to_owned(),
            supports_cancellation: true,
            replay_safety: ReplaySafety::Unknown,
        }
    }

    #[test]
    fn only_a_tool_its_app_says_only_reads_is_offered() {
        assert!(offerable(&tool(RiskClass::Read, SideEffect::None)));
        // Everything else an App tool can be described as is registered and
        // asked about if called, but never put in front of a model.
        for (risk, effect) in [
            (RiskClass::Read, SideEffect::LocalReversible),
            (RiskClass::Read, SideEffect::External),
            (RiskClass::Modify, SideEffect::None),
            (RiskClass::Modify, SideEffect::LocalReversible),
            (RiskClass::Dangerous, SideEffect::None),
            (RiskClass::Dangerous, SideEffect::LocalIrreversible),
        ] {
            assert!(!offerable(&tool(risk, effect)), "{risk:?} {effect:?}");
        }
    }
}
