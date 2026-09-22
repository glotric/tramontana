//! Configuring an App and seeing what it offers (ADR 0036).
//!
//! Against `tramontana-fake-mcp-server`, the stand-in the MCP adapter's own
//! tests use. It lives beside this binary in the build directory; the
//! `CARGO_BIN_EXE_` variable exists only in the crate that declares it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::path::PathBuf;
use std::process::Command;

fn fake_server() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tramontana"))
        .parent()
        .expect("the test binary has a directory")
        .join(format!(
            "tramontana-fake-mcp-server{}",
            std::env::consts::EXE_SUFFIX
        ))
}

/// Run the CLI against `profile`, returning what it printed on both streams
/// and whether it succeeded. Both streams, so a failure says why.
fn cli(profile: &std::path::Path, args: &[&str]) -> (String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_tramontana"))
        .args(args)
        .arg("--profile")
        .arg(profile)
        .output()
        .expect("the CLI runs");
    let mut printed = String::from_utf8_lossy(&output.stdout).into_owned();
    printed.push_str(&String::from_utf8_lossy(&output.stderr));
    (printed, output.status.success())
}

#[test]
fn an_app_is_configured_listed_and_asked_what_it_offers() {
    assert!(
        fake_server().exists(),
        "build the fixture server first: {}",
        fake_server().display()
    );
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");

    let (added, ok) = cli(
        &profile,
        &[
            "app",
            "add",
            "notes",
            "--program",
            fake_server().to_str().unwrap(),
            "--arg",
            "ok",
        ],
    );
    assert!(ok, "{added}");
    assert!(added.contains("mcp.notes.<tool>"), "{added}");

    let (listed, ok) = cli(&profile, &["app", "list"]);
    assert!(ok, "{listed}");
    assert!(listed.contains("notes"), "{listed}");
    assert!(listed.contains("enabled"), "{listed}");

    let (tools, ok) = cli(&profile, &["app", "tools", "notes"]);
    assert!(ok, "{tools}");
    assert!(tools.contains("answered on MCP 2025-11-25"), "{tools}");
    // What it says it does decides what may be offered, not what may run.
    assert!(tools.contains("mcp.notes.greeting"), "{tools}");
    assert!(tools.contains("says it only reads"), "{tools}");
    assert!(tools.contains("mcp.notes.stamp"), "{tools}");
    assert!(tools.contains("not offered to a model"), "{tools}");
}

#[test]
fn a_secret_is_named_for_the_credential_store_never_given_here() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");

    let (added, ok) = cli(
        &profile,
        &[
            "app",
            "add",
            "notes",
            "--program",
            fake_server().to_str().unwrap(),
            "--secret",
            "API_TOKEN=notes.token",
        ],
    );

    assert!(ok, "{added}");
    // The key is named and the person is told where its value belongs; the
    // value itself never passes through here.
    assert!(
        added.contains("API_TOKEN from the credential store"),
        "{added}"
    );
    assert!(
        added.contains("tramontana secret set notes.token"),
        "{added}"
    );
}

#[test]
fn an_app_can_be_turned_off_and_removed() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");
    let (_, ok) = cli(
        &profile,
        &[
            "app",
            "add",
            "notes",
            "--program",
            fake_server().to_str().unwrap(),
        ],
    );
    assert!(ok);

    let (off, ok) = cli(&profile, &["app", "enable", "notes", "--off"]);
    assert!(ok, "{off}");
    let (listed, _) = cli(&profile, &["app", "list"]);
    assert!(listed.contains("disabled"), "{listed}");

    let (removed, ok) = cli(&profile, &["app", "remove", "notes"]);
    assert!(ok, "{removed}");
    let (listed, _) = cli(&profile, &["app", "list"]);
    assert!(listed.contains("no Apps are configured"), "{listed}");
}

#[test]
fn an_identifier_that_could_leave_its_namespace_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("profile.sqlite");
    for bad in ["../escape", "Notes", "notes.two"] {
        let (_, ok) = cli(
            &profile,
            &[
                "app",
                "add",
                bad,
                "--program",
                fake_server().to_str().unwrap(),
            ],
        );
        assert!(!ok, "{bad} was accepted");
    }
    // Nor a program the shell would have to find.
    let (_, ok) = cli(
        &profile,
        &[
            "app",
            "add",
            "notes",
            "--program",
            "tramontana-fake-mcp-server",
        ],
    );
    assert!(!ok, "a relative program was accepted");
}
