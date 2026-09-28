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
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.command {
        ProfileCommand::Show => service.personal_profile().await,
        ProfileCommand::Set(set) => set_details(&service, set).await,
        ProfileCommand::Remember { text } => service.remember(&text).await,
        ProfileCommand::Forget { note_id } => service.forget(&note_id).await,
    };
    match result {
        Ok(profile) => render::json(&profile),
        Err(error) => render::application_error(&error),
    }
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
