//! Dictation with a `whisper-cli` the person supplies (ADR 0065).
//!
//! Supplied like `llama-server`: chosen by path, fingerprinted when chosen,
//! held if its executable changes (ADR 0043), started with a cleared
//! environment, and never downloaded. Each transcription writes the audio to
//! a fresh temporary file, runs the program once under a deadline, and
//! removes the file whatever happens.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tramontana_core::contracts::{
    CorrelationId, DictationSetup, ErrorCode, ErrorEnvelope, RuntimeProvenance, Timestamp,
};
use tramontana_core::ports::Transcriber;

use crate::fingerprint::{fingerprint_file, fingerprint_model};
use crate::llama::base_command;

/// How long one transcription may take.
pub const TRANSCRIBE_DEADLINE: Duration = Duration::from_secs(120);

/// The most text a transcription may print.
const OUTPUT_BYTES: usize = 256 * 1024;

fn failure(code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("dictation"))
}

/// Transcribes with the `whisper-cli` in a [`DictationSetup`].
#[derive(Debug, Default, Clone)]
pub struct WhisperCli {
    temporary: Option<PathBuf>,
}

impl WhisperCli {
    /// A transcriber writing its audio to the system's temporary folder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A transcriber writing its audio under `directory` instead.
    #[must_use]
    pub fn in_directory(directory: PathBuf) -> Self {
        Self {
            temporary: Some(directory),
        }
    }
}

/// Removes the audio file when dropped, however transcription ended.
struct Audio(PathBuf);

impl Drop for Audio {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[async_trait]
impl Transcriber for WhisperCli {
    fn inspect(
        &self,
        binary_path: &str,
        model_path: &str,
        recorded_at: Timestamp,
    ) -> Result<RuntimeProvenance, ErrorEnvelope> {
        let executable = fingerprint_file(Path::new(binary_path)).map_err(|error| {
            failure(
                ErrorCode::InvalidRequest,
                format!("The whisper.cpp program could not be read: {error}"),
            )
        })?;
        let model = fingerprint_model(Path::new(model_path)).map_err(|error| {
            failure(
                ErrorCode::InvalidRequest,
                format!("The whisper model could not be read: {error}"),
            )
        })?;
        Ok(RuntimeProvenance {
            executable_sha256: executable.sha256,
            executable_bytes: executable.bytes,
            model_sha256: model.sha256,
            model_bytes: model.bytes,
            recorded_at,
        })
    }

    async fn transcribe(
        &self,
        setup: &DictationSetup,
        wav: Vec<u8>,
    ) -> Result<String, ErrorEnvelope> {
        let binary = Path::new(&setup.binary_path);
        // Held if it changed since it was chosen (ADR 0043).
        let now = fingerprint_file(binary).map_err(|_| {
            failure(
                ErrorCode::WorkerUnavailable,
                "The whisper.cpp program can no longer be read; choose it again in Settings",
            )
        })?;
        if now.sha256 != setup.provenance.executable_sha256 {
            return Err(failure(
                ErrorCode::WorkerUnavailable,
                "The whisper.cpp program changed since you chose it; choose it again in Settings to use it",
            ));
        }

        let directory = self.temporary.clone().unwrap_or_else(std::env::temp_dir);
        let audio =
            Audio(directory.join(format!("tramontana-dictation-{}.wav", uuid::Uuid::new_v4())));
        std::fs::write(&audio.0, &wav).map_err(|error| {
            failure(
                ErrorCode::Internal,
                format!("The recording could not be handed to whisper.cpp: {error}"),
            )
        })?;

        let mut command = base_command(binary);
        command
            .arg("-m")
            .arg(&setup.model_path)
            .arg("-f")
            .arg(&audio.0)
            .arg("-l")
            .arg(setup.language.as_deref().unwrap_or("auto"))
            // No timestamps, no progress: only the words, on standard output.
            .arg("-nt")
            .arg("-np")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command.spawn().map_err(|error| {
            failure(
                ErrorCode::WorkerUnavailable,
                format!("whisper.cpp could not be started: {error}"),
            )
        })?;
        let output = match tokio::time::timeout(TRANSCRIBE_DEADLINE, child.wait_with_output()).await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return Err(failure(
                    ErrorCode::WorkerUnavailable,
                    format!("whisper.cpp stopped unexpectedly: {error}"),
                ));
            }
            // Dropping the child on the way out kills it.
            Err(_) => {
                return Err(failure(
                    ErrorCode::Timeout,
                    "whisper.cpp took too long; a smaller model is faster",
                ));
            }
        };
        drop(audio);
        if !output.status.success() {
            return Err(failure(
                ErrorCode::WorkerUnavailable,
                "whisper.cpp could not transcribe the recording; check the model in Settings",
            ));
        }
        let mut stdout = output.stdout;
        stdout.truncate(OUTPUT_BYTES);
        let text = String::from_utf8_lossy(&stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if text.is_empty() {
            return Err(failure(
                ErrorCode::InvalidRequest,
                "No speech was heard in the recording",
            ));
        }
        Ok(text)
    }
}
