//! `tramontana profile erase`, across processes (ADR 0068).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests fail loudly on isolated synthetic fixtures"
)]

use std::path::Path;
use std::process::{Command, Output};

fn cli(profile: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tramontana"))
        .args(arguments)
        .arg("--profile")
        .arg(profile)
        .output()
        .expect("the CLI binary should start")
}

fn projects(profile: &Path) -> usize {
    let output = cli(profile, &["project", "list"]);
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .unwrap()
        .as_array()
        .map_or(0, Vec::len)
}

#[test]
fn erasing_needs_yes_and_then_leaves_an_empty_profile_and_the_files() {
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("notes.md"), "mine").unwrap();
    let profile = directory.path().join("profile.sqlite");
    let opened = cli(&profile, &["project", "open", project.to_str().unwrap()]);
    assert!(opened.status.success(), "{opened:?}");
    assert_eq!(projects(&profile), 1);

    // Without --yes nothing happens.
    let refused = cli(&profile, &["profile", "erase"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    assert_eq!(projects(&profile), 1);

    let erased = cli(&profile, &["profile", "erase", "--yes"]);
    assert!(erased.status.success(), "{erased:?}");
    let report: serde_json::Value = serde_json::from_slice(&erased.stdout).unwrap();
    assert_eq!(report["erased"], true);

    assert_eq!(projects(&profile), 0);
    assert_eq!(
        std::fs::read_to_string(project.join("notes.md")).unwrap(),
        "mine"
    );
}
