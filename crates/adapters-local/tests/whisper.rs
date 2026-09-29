//! The `whisper-cli` transcriber against stand-ins (ADR 0065).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good fixtures; a panic is the failure report"
)]

use tramontana_adapters_local::WhisperCli;
use tramontana_core::contracts::{DictationSetup, ErrorCode, Timestamp};
use tramontana_core::ports::Transcriber;

fn setup_for(
    transcriber: &WhisperCli,
    binary: &std::path::Path,
    model: &std::path::Path,
) -> DictationSetup {
    DictationSetup {
        binary_path: binary.display().to_string(),
        model_path: model.display().to_string(),
        language: Some("en".into()),
        provenance: transcriber
            .inspect(
                &binary.display().to_string(),
                &model.display().to_string(),
                Timestamp::new("2026-09-29T00:00:00.000Z"),
            )
            .unwrap(),
    }
}

#[tokio::test]
async fn a_changed_executable_is_held_and_never_run() {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("whisper-cli");
    let model = directory.path().join("ggml-base.bin");
    std::fs::write(&binary, "one version").unwrap();
    std::fs::write(&model, "weights").unwrap();
    let transcriber = WhisperCli::in_directory(directory.path().to_path_buf());
    let setup = setup_for(&transcriber, &binary, &model);

    std::fs::write(&binary, "another version").unwrap();
    let held = transcriber
        .transcribe(&setup, b"RIFF".to_vec())
        .await
        .unwrap_err();
    assert_eq!(held.code, ErrorCode::WorkerUnavailable);
    assert!(held.message.contains("changed since you chose it"));
    // Refused before any audio was written.
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn files_that_cannot_be_read_are_not_set_up() {
    let transcriber = WhisperCli::new();
    let refused = transcriber
        .inspect(
            "/nowhere/whisper-cli",
            "/nowhere/model.bin",
            Timestamp::new("t"),
        )
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
}

#[cfg(unix)]
mod running {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in `whisper-cli`: records its arguments and environment, and
    /// prints what it was built to "hear".
    fn stand_in(directory: &std::path::Path, script: &str) -> std::path::PathBuf {
        let binary = directory.join("whisper-cli");
        std::fs::write(&binary, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        binary
    }

    #[tokio::test]
    async fn the_words_are_returned_and_the_audio_file_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("seen.txt");
        let binary = stand_in(
            directory.path(),
            &format!(
                "echo \"$@\" > {log}; env >> {log}; echo ' Plan my week.'; echo ''; echo ' Thank you.'",
                log = log.display()
            ),
        );
        let model = directory.path().join("ggml-base.bin");
        std::fs::write(&model, "weights").unwrap();
        let audio_dir = tempfile::tempdir().unwrap();
        let transcriber = WhisperCli::in_directory(audio_dir.path().to_path_buf());
        let setup = setup_for(&transcriber, &binary, &model);

        let text = transcriber
            .transcribe(&setup, b"RIFF....WAVE".to_vec())
            .await
            .unwrap();
        assert_eq!(text, "Plan my week. Thank you.");

        let seen = std::fs::read_to_string(&log).unwrap();
        assert!(seen.contains(&format!("-m {}", model.display())));
        assert!(seen.contains("-l en -nt -np"));
        assert!(
            !seen.contains("OPENAI_API_KEY") && !seen.contains("HOME="),
            "the environment is cleared: {seen}"
        );
        assert_eq!(
            std::fs::read_dir(audio_dir.path()).unwrap().count(),
            0,
            "the recording is removed after use"
        );
    }

    #[tokio::test]
    async fn a_failing_or_silent_run_says_so_and_still_removes_the_audio() {
        let directory = tempfile::tempdir().unwrap();
        let model = directory.path().join("ggml-base.bin");
        std::fs::write(&model, "weights").unwrap();
        let audio_dir = tempfile::tempdir().unwrap();
        let transcriber = WhisperCli::in_directory(audio_dir.path().to_path_buf());

        let failing = stand_in(directory.path(), "exit 3");
        let setup = setup_for(&transcriber, &failing, &model);
        let error = transcriber
            .transcribe(&setup, b"RIFF".to_vec())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::WorkerUnavailable);

        let silent = stand_in(directory.path(), "echo '   '");
        let setup = setup_for(&transcriber, &silent, &model);
        let error = transcriber
            .transcribe(&setup, b"RIFF".to_vec())
            .await
            .unwrap_err();
        assert!(error.message.contains("No speech"));
        assert_eq!(std::fs::read_dir(audio_dir.path()).unwrap().count(), 0);
    }
}
