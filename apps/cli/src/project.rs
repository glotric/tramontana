//! Durable project commands.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use tramontana_core::config::ConfigurationSelection;
use tramontana_core::contracts::ProjectId;
use tramontana_platform::canonicalize_root;

use crate::{configuration, render, wiring};

/// Arguments for `tramontana project`.
#[derive(Debug, Args)]
pub(crate) struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Open a canonical project root, creating its durable record if needed.
    Open(OpenProjectArgs),
    /// List recent durable projects.
    List(ProfileArgs),
    /// Inspect one project and its latest resolved configuration.
    Inspect(InspectProjectArgs),
    /// Remove a project from Tramontana: its conversations, tasks, records
    /// and schedules. The folder and its files are not touched.
    Delete(DeleteProjectArgs),
}

#[derive(Debug, Args)]
struct DeleteProjectArgs {
    #[arg(value_name = "PROJECT_ID")]
    project_id: String,

    /// Confirm the removal. Without it, nothing is removed.
    #[arg(long)]
    yes: bool,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct OpenProjectArgs {
    /// Directory to approve as the project root.
    #[arg(value_name = "PATH")]
    path: PathBuf,

    /// Display name. Defaults to the directory name.
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct InspectProjectArgs {
    #[arg(value_name = "PROJECT_ID")]
    project_id: String,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

pub(crate) async fn execute(args: ProjectArgs) -> ExitCode {
    match args.command {
        ProjectCommand::Open(args) => open(args).await,
        ProjectCommand::List(args) => list(args).await,
        ProjectCommand::Inspect(args) => inspect(args).await,
        ProjectCommand::Delete(args) => delete(args).await,
    }
}

async fn open(args: OpenProjectArgs) -> ExitCode {
    let root = match canonicalize_root(&args.path) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("error: could not open project root: {error}");
            return ExitCode::FAILURE;
        }
    };
    let name = args.name.unwrap_or_else(|| {
        root.as_path()
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Untitled project")
            .to_owned()
    });
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    let project = match service
        .open_project(name, vec![root.as_str().to_owned()])
        .await
    {
        Ok(project) => project,
        Err(error) => return render::application_error(&error),
    };
    let configuration = match configuration::resolve_project(
        &service,
        &project.project_id,
        &ConfigurationSelection::default(),
    )
    .await
    {
        Ok(configuration) => configuration,
        Err(message) => {
            eprintln!("error: project opened, but configuration could not be resolved: {message}");
            return ExitCode::FAILURE;
        }
    };

    render::json(&serde_json::json!({
        "project": project,
        "configuration": configuration,
    }))
}

async fn list(args: ProfileArgs) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    match service.list_projects().await {
        Ok(projects) => render::json(&projects),
        Err(error) => render::application_error(&error),
    }
}

async fn inspect(args: InspectProjectArgs) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    let project_id = ProjectId::new(args.project_id);
    let project = match service.get_project(&project_id).await {
        Ok(project) => project,
        Err(error) => return render::application_error(&error),
    };
    let configuration = match service.get_effective_configuration(&project_id).await {
        Ok(configuration) => configuration,
        Err(error) => return render::application_error(&error),
    };
    render::json(&serde_json::json!({
        "project": project,
        "configuration": configuration,
    }))
}

async fn delete(args: DeleteProjectArgs) -> ExitCode {
    if !args.yes {
        eprintln!(
            "error: removing a project deletes its conversations, tasks and records from this profile, and cannot be undone (its files are not touched); pass --yes to confirm"
        );
        return ExitCode::FAILURE;
    }
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    match service
        .delete_project(&tramontana_core::contracts::ProjectId::new(args.project_id))
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => render::application_error(&error),
    }
}
