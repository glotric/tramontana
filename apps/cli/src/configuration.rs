//! Project configuration discovery and inspection commands.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::{EffectiveConfiguration, ProjectId};
use tramontana_core::services::TaskService;
use tramontana_platform::{canonicalize_root, discover};

use crate::{render, wiring};

/// Arguments for `tramontana configuration`.
#[derive(Debug, Args)]
pub(crate) struct ConfigurationArgs {
    #[command(subcommand)]
    command: ConfigurationCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigurationCommand {
    /// Re-discover, resolve, persist, and print effective project configuration.
    Inspect(InspectConfigurationArgs),
}

#[derive(Debug, Args)]
struct InspectConfigurationArgs {
    /// Project whose approved root should be inspected.
    #[arg(value_name = "PROJECT_ID")]
    project_id: String,

    /// Project-relative path that determines scoped instruction activation.
    #[arg(long, value_name = "RELATIVE_PATH")]
    active_path: Option<String>,

    /// Agent profile definition to activate.
    #[arg(long, value_name = "ID")]
    agent_id: Option<String>,

    /// Workflow definition to activate.
    #[arg(long, value_name = "ID")]
    workflow_id: Option<String>,

    /// Skill definition to activate. Repeat to set explicit precedence order.
    #[arg(long = "skill-id", value_name = "ID")]
    skill_ids: Vec<String>,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

pub(crate) async fn execute(args: ConfigurationArgs) -> ExitCode {
    match args.command {
        ConfigurationCommand::Inspect(args) => inspect(args).await,
    }
}

async fn inspect(args: InspectConfigurationArgs) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    let project_id = ProjectId::new(args.project_id);
    let selection = ConfigurationSelection {
        active_path: args.active_path,
        agent_id: args.agent_id,
        workflow_id: args.workflow_id,
        skill_ids: args.skill_ids,
    };

    match resolve_project(&service, &project_id, &selection).await {
        Ok(configuration) => render::json(&serde_json::json!({
            "projectId": project_id,
            "configuration": configuration,
        })),
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Discover and resolve configuration through the same platform and core
/// boundaries used by every CLI workflow.
pub(crate) async fn resolve_project(
    service: &TaskService,
    project_id: &ProjectId,
    selection: &ConfigurationSelection,
) -> Result<EffectiveConfiguration, String> {
    let project = service
        .get_project(project_id)
        .await
        .map_err(|error| format!("{:?}: {}", error.code, error.message))?;
    let [root] = project.roots.as_slice() else {
        return Err(format!(
            "project {project_id} has {} roots; M2 configuration discovery supports the default single-root project case",
            project.roots.len()
        ));
    };
    let root = canonicalize_root(Path::new(root))
        .map_err(|error| format!("could not reopen the approved project root: {error}"))?;
    service
        .resolve_configuration(project_id, discover(&root), selection)
        .await
        .map_err(|error| format!("{:?}: {}", error.code, error.message))
}
