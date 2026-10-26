//! Portable export and import.
//!
//! Export is a data-release action, so every command here shows what is being
//! released and what was withheld. Import is the opposite: it treats the
//! bundle as untrusted, validates it before believing any of it, and runs
//! nothing it contains.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use tramontana_core::contracts::{ExportOptions, ProjectId, TaskId};
use tramontana_core::services::{ImportLimits, ImportSummary};

use crate::{render, wiring};

/// Arguments for `tramontana bundle`.
#[derive(Debug, Args)]
pub(crate) struct BundleArgs {
    #[command(subcommand)]
    command: BundleCommand,
}

#[derive(Debug, Subcommand)]
enum BundleCommand {
    /// Write a portable bundle for a project, a task, or projectless work.
    Export(ExportArgs),
    /// Validate a bundle and report what it holds, writing nothing.
    Inspect(BundlePathArgs),
    /// Validate a bundle and write its records into this profile.
    Import(BundlePathArgs),
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Directory to write the bundle into. Created if it does not exist.
    #[arg(value_name = "DIRECTORY")]
    directory: PathBuf,

    /// Export one project. Omit both selectors to export projectless work.
    #[arg(long, value_name = "PROJECT_ID", conflicts_with = "task_id")]
    project_id: Option<String>,

    /// Export a single task.
    #[arg(long, value_name = "TASK_ID")]
    task_id: Option<String>,

    /// Keep absolute device paths, which name this machine.
    #[arg(long)]
    include_absolute_paths: bool,

    /// Keep raw tool arguments and results, which can contain project/file contents.
    #[arg(long)]
    include_raw_tool_output: bool,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct BundlePathArgs {
    /// Directory holding `manifest.json`.
    #[arg(value_name = "DIRECTORY")]
    directory: PathBuf,

    /// Database file to use.
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
}

pub(crate) async fn execute(args: BundleArgs) -> ExitCode {
    match args.command {
        BundleCommand::Export(args) => export(args).await,
        BundleCommand::Inspect(args) => inspect(args).await,
        BundleCommand::Import(args) => import(args).await,
    }
}

async fn export(args: ExportArgs) -> ExitCode {
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };

    let options = ExportOptions {
        include_absolute_paths: args.include_absolute_paths,
        include_raw_tool_output: args.include_raw_tool_output,
        include_artifacts: false,
    };

    let bundle = match args.task_id {
        Some(task_id) => service.export_task(&TaskId::new(task_id), options).await,
        None => {
            let project_id = args.project_id.map(ProjectId::new);
            service.export_project(project_id.as_ref(), options).await
        }
    };
    let bundle = match bundle {
        Ok(bundle) => bundle,
        Err(error) => return render::application_error(&error),
    };

    if let Err(error) = tramontana_platform::write_bundle(&bundle, &args.directory) {
        eprintln!("error: could not write the bundle: {error}");
        return ExitCode::FAILURE;
    }

    render::json(&serde_json::json!({
        "directory": args.directory,
        "manifest": bundle.manifest,
    }))
}

async fn inspect(args: BundlePathArgs) -> ExitCode {
    let (service, bundle) = match load(&args).await {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };

    match service.stage_import(&bundle, ImportLimits::default()) {
        Ok(records) => render::json(&serde_json::json!({
            "manifest": bundle.manifest,
            "contains": ImportSummary::from(&records),
            "wroteAnything": false,
        })),
        Err(error) => render::application_error(&error),
    }
}

async fn import(args: BundlePathArgs) -> ExitCode {
    let (service, bundle) = match load(&args).await {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };

    match service
        .import_bundle(&bundle, ImportLimits::default())
        .await
    {
        Ok(summary) => render::json(&serde_json::json!({
            "imported": summary,
            // Stated rather than assumed: an imported task is a record, and
            // nothing in the bundle was executed to produce it.
            "executedAnything": false,
        })),
        Err(error) => render::application_error(&error),
    }
}

/// Open the profile and read the bundle, before anything is believed about it.
async fn load(
    args: &BundlePathArgs,
) -> Result<
    (
        std::sync::Arc<tramontana_core::services::TaskService>,
        tramontana_core::contracts::ExportBundle,
    ),
    ExitCode,
> {
    let service = match wiring::open(&wiring::profile_path(args.profile.clone()), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return Err(ExitCode::FAILURE);
        }
    };
    let bundle = match tramontana_platform::read_bundle(&args.directory) {
        Ok(bundle) => bundle,
        Err(error) => {
            eprintln!("error: could not read the bundle: {error}");
            return Err(ExitCode::FAILURE);
        }
    };
    Ok((service, bundle))
}
