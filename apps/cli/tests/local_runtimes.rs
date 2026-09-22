//! Local runtimes Tramontana starts, configured from the CLI: an
//! `mlx_lm.server` (ADR 0010), and the record of their files (ADR 0043).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated integration fixture assertions"
)]

mod support;

use support::cli;

#[test]
fn an_mlx_runtime_is_configured_as_one_tramontana_starts_on_this_device() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");
    let folder = directory.path().join("Qwen-MLX");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("config.json"), b"{}").unwrap();
    let binary = directory.path().join("mlx_lm.server");
    std::fs::write(&binary, b"#!/bin/sh\n").unwrap();

    cli(
        &profile,
        &[
            "provider",
            "add-mlx",
            "mlx",
            "--binary",
            binary.to_str().unwrap(),
            "--model-path",
            folder.to_str().unwrap(),
            "--model",
            "qwen-mlx",
        ],
    );

    let listed = String::from_utf8(cli(&profile, &["provider", "list"]).stdout).unwrap();
    let line = listed
        .lines()
        .find(|line| line.starts_with("mlx "))
        .unwrap_or_else(|| panic!("{listed}"));
    assert!(line.contains("mlxServer"), "{line}");
    assert!(line.contains("qwen-mlx"), "{line}");
    // Derived, not declared: a runtime Tramontana starts is on this device.
    assert!(!line.contains("CLOUD"), "{line}");
}

/// ADR 0043, from the CLI: files are recorded when a runtime is added, a
/// changed executable shows as held, `check` says what changed, and `trust`
/// records them as they are now.
#[test]
fn a_runtimes_files_are_recorded_checked_held_and_confirmed() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");
    let binary = directory.path().join("llama-server");
    let model = directory.path().join("model.gguf");
    std::fs::write(&binary, b"version one").unwrap();
    std::fs::write(&model, b"weights").unwrap();
    let path = |p: &std::path::Path| p.to_str().unwrap().to_owned();

    cli(
        &profile,
        &[
            "provider",
            "add-llama",
            "llama",
            "--binary",
            &path(&binary),
            "--model-path",
            &path(&model),
            "--model",
            "m",
        ],
    );
    let listed = |profile| String::from_utf8(cli(profile, &["provider", "list"]).stdout).unwrap();
    assert!(
        listed(&profile).contains("files=confirmed"),
        "{}",
        listed(&profile)
    );

    // An upgrade replaces the executable: it is held.
    std::fs::write(&binary, b"version two").unwrap();
    assert!(
        listed(&profile).contains("files=HELD(executable-changed)"),
        "{}",
        listed(&profile)
    );
    // A changed model is reported by `check`, which reads the whole model.
    std::fs::write(&model, b"other weights").unwrap();
    let checked = String::from_utf8(cli(&profile, &["provider", "check", "llama"]).stdout).unwrap();
    assert!(
        checked.contains("executable: HELD(executable-changed)"),
        "{checked}"
    );
    assert!(checked.contains("model: CHANGED"), "{checked}");

    // Confirmed as they are now.
    cli(&profile, &["provider", "trust", "llama"]);
    assert!(
        listed(&profile).contains("files=confirmed"),
        "{}",
        listed(&profile)
    );
    let checked = String::from_utf8(cli(&profile, &["provider", "check", "llama"]).stdout).unwrap();
    assert!(checked.contains("model: as recorded"), "{checked}");
}

#[test]
fn a_runtime_whose_files_cannot_be_read_is_not_added() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tramontana"))
        .args([
            "provider",
            "add-llama",
            "llama",
            "--binary",
            directory.path().join("absent").to_str().unwrap(),
            "--model-path",
            directory.path().join("absent.gguf").to_str().unwrap(),
            "--model",
            "m",
            "--profile",
            profile.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("could not read the executable"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
