//! The person's own skills (ADR 0070).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::{render, wiring};

/// Arguments for `tramontana skill`.
#[derive(Debug, Args)]
pub(crate) struct SkillArgs {
    #[command(subcommand)]
    command: SkillCommand,

    /// Database file to use; the skills are kept in a folder beside it.
    #[arg(long, value_name = "PATH", global = true)]
    profile: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    /// List your skills, as JSON.
    List,
    /// Import a skill: a folder with a SKILL.md, a .zip holding one, or a .md
    /// file. Only the SKILL.md is read; nothing in it is run.
    Import {
        /// The skill folder or file.
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },
    /// Delete one of your skills. Tasks that used it keep what they read.
    Remove {
        /// The skill's id, as `skill list` shows it.
        #[arg(value_name = "ID")]
        id: String,
        /// Confirm the deletion.
        #[arg(long)]
        yes: bool,
    },
}

pub(crate) async fn execute(args: SkillArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.command {
        SkillCommand::List => service.personal_skills().await.map(|s| render::json(&s)),
        SkillCommand::Import { path } => {
            let (name, markdown) = match tramontana_platform::read_skill_source(&path) {
                Ok(read) => read,
                Err(error) => {
                    eprintln!("error: could not import {}: {error}", path.display());
                    return ExitCode::FAILURE;
                }
            };
            service
                .import_skill(&markdown, &name)
                .await
                .map(|s| render::json(&s))
        }
        SkillCommand::Remove { id, yes } => {
            if !yes {
                eprintln!("error: this deletes the skill \"{id}\"; pass --yes to confirm");
                return ExitCode::FAILURE;
            }
            service.remove_skill(&id).await.map(|()| ExitCode::SUCCESS)
        }
    };
    result.unwrap_or_else(|error| render::application_error(&error))
}
