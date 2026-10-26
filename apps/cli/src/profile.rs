//! The person's profile and memory (ADR 0061).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand, ValueEnum};
use tramontana_core::contracts::{AnswerStyle, PersonalDetails};
use tramontana_core::services::TaskService;

use crate::{render, wiring};

/// Arguments for `tramontana profile`.
#[derive(Debug, Args)]
pub(crate) struct ProfileArgs {
    #[command(subcommand)]
    command: ProfileCommand,

    /// Database file to use.
    #[arg(long, value_name = "PATH", global = true)]
    profile: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Print the profile and its notes as JSON.
    Show,
    /// Change what chats are told about you. Omitted fields keep their value.
    Set(SetArgs),
    /// Add a note for chats to remember.
    Remember {
        /// The note.
        text: String,
    },
    /// Remove a note, by its id from `profile show`.
    Forget {
        /// The note's id.
        note_id: String,
    },
    /// Delete everything Tramontana keeps for this profile: projects list,
    /// conversations, tasks, settings, Apps, notes, and the keys they name
    /// in the operating system's store. Project folders are not touched.
    Erase {
        /// Confirm the deletion. Without it, nothing is deleted.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum StyleArg {
    Quick,
    Balanced,
    Thorough,
}

#[derive(Debug, Args)]
struct SetArgs {
    /// What chats should call you. An empty value clears it.
    #[arg(long)]
    name: Option<String>,
    /// A sentence or two about your work. An empty value clears it.
    #[arg(long)]
    about: Option<String>,
    /// How answers should be written.
    #[arg(long, value_enum)]
    style: Option<StyleArg>,
    /// Whether chats are given your notes.
    #[arg(long)]
    memory: Option<bool>,
}

pub(crate) async fn execute(args: ProfileArgs) -> ExitCode {
    if let ProfileCommand::Erase { yes } = args.command {
        return erase(args.profile, yes).await;
    }
    let Some(service) = wiring::open_or_report(args.profile).await else {
        return ExitCode::FAILURE;
    };
    let result = match args.command {
        ProfileCommand::Show => service.personal_profile().await,
        ProfileCommand::Set(set) => set_details(&service, set).await,
        ProfileCommand::Remember { text } => service.remember(&text).await,
        ProfileCommand::Forget { note_id } => service.forget(&note_id).await,
        ProfileCommand::Erase { .. } => unreachable!("handled before the profile is opened"),
    };
    render::result(result)
}

async fn set_details(
    service: &TaskService,
    set: SetArgs,
) -> Result<tramontana_core::contracts::PersonalProfile, tramontana_core::contracts::ErrorEnvelope>
{
    let current = service.personal_profile().await?;
    let details = PersonalDetails {
        name: set.name.unwrap_or_else(|| current.name.unwrap_or_default()),
        about: set
            .about
            .unwrap_or_else(|| current.about.unwrap_or_default()),
        answer_style: set.style.map_or(current.answer_style, |style| match style {
            StyleArg::Quick => AnswerStyle::Quick,
            StyleArg::Balanced => AnswerStyle::Balanced,
            StyleArg::Thorough => AnswerStyle::Thorough,
        }),
        memory_enabled: set.memory.unwrap_or(current.memory_enabled),
    };
    service.update_personal(&details).await
}

/// `tramontana profile erase` (ADR 0068).
async fn erase(profile: Option<PathBuf>, yes: bool) -> ExitCode {
    if !yes {
        eprintln!(
            "error: this deletes every project record, conversation, task, setting and note in this profile, and the keys they name, and cannot be undone (project folders are not touched); pass --yes to confirm"
        );
        return ExitCode::FAILURE;
    }
    match wiring::erase(&wiring::profile_path(profile)).await {
        Ok(report) => render::json(&serde_json::json!({
            "erased": true,
            "credentialsRemoved": report.credentials_removed,
            "credentialsAbsent": report.credentials_absent,
            "skillsRemoved": report.skills_removed,
        })),
        Err(error) => render::application_error(&error),
    }
}
