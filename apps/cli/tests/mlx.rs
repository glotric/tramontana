//! An `mlx_lm.server` endpoint, configured from the CLI (ADR 0010).

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

    cli(
        &profile,
        &[
            "provider",
            "add-mlx",
            "mlx",
            "--binary",
            directory.path().join("mlx_lm.server").to_str().unwrap(),
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
