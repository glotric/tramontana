//! Dictation with a whisper.cpp you supply (ADR 0065).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};

use crate::{render, wiring};

/// Arguments for `tramontana dictation`.
#[derive(Debug, Args)]
pub(crate) struct DictationArgs {
    #[command(subcommand)]
    command: DictationCommand,

    /// Database file to use.
    #[arg(long, value_name = "PATH", global = true)]
    profile: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum DictationCommand {
    /// Say whether dictation is set up, as JSON.
    Status,
    /// Choose the whisper-cli program and model. Nothing is downloaded.
    Setup {
        /// The whisper-cli executable.
        #[arg(long, value_name = "PATH")]
        binary: PathBuf,
        /// The model file, such as ggml-base.bin.
        #[arg(long, value_name = "PATH")]
        model: PathBuf,
        /// A language code such as en or sl. Omit for automatic.
        #[arg(long, value_name = "CODE")]
        language: Option<String>,
    },
    /// Forget the setup. The files are not touched.
    Clear,
    /// Print the text spoken in a 16 kHz mono WAV file.
    Transcribe {
        /// The recording.
        #[arg(value_name = "WAV")]
        audio: PathBuf,
    },
}

pub(crate) async fn execute(args: DictationArgs) -> ExitCode {
    let service = match wiring::open(&wiring::profile_path(args.profile), 0).await {
        Ok(service) => service,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.command {
        DictationCommand::Status => service.dictation_status().await.map(|s| render::json(&s)),
        DictationCommand::Setup {
            binary,
            model,
            language,
        } => service
            .set_dictation(
                &binary.display().to_string(),
                &model.display().to_string(),
                language.as_deref(),
            )
            .await
            .map(|s| render::json(&s)),
        DictationCommand::Clear => service.clear_dictation().await.map(|s| render::json(&s)),
        DictationCommand::Transcribe { audio } => {
            let bytes = match std::fs::read(&audio) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!("error: could not read {}: {error}", audio.display());
                    return ExitCode::FAILURE;
                }
            };
            service.transcribe(bytes).await.map(|text| {
                println!("{text}");
                ExitCode::SUCCESS
            })
        }
    };
    result.unwrap_or_else(|error| render::application_error(&error))
}
