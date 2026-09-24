//! A task uses the project's skills it names, and no others (ADR 0053).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration tests fail loudly on isolated synthetic fixtures"
)]

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn text(value: &str) -> OsString {
    OsString::from(value)
}

fn run(arguments: Vec<OsString>) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_tramontana"))
        .args(arguments)
        .output()
        .expect("the CLI binary should start");
    assert!(
        output.status.success(),
        "CLI failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn profile_argument(profile: &Path) -> Vec<OsString> {
    vec![text("--profile"), profile.as_os_str().to_owned()]
}

/// Run one task and return the context manifest it recorded.
fn manifest_of_task(profile: &Path, conversation_id: &str, skill: Option<&str>) -> Value {
    let mut arguments = vec![
        text("task"),
        text("Say what the project asks for"),
        text("--conversation-id"),
        text(conversation_id),
        text("--chunk-delay-ms"),
        text("0"),
        text("--json"),
    ];
    if let Some(skill) = skill {
        arguments.push(text("--skill-id"));
        arguments.push(text(skill));
    }
    arguments.extend(profile_argument(profile));

    let output = run(arguments);
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["kind"] == "contextPrepared")
        .expect("a task records the context it prepared")["payload"]["manifest"]
        .clone()
}

/// The manifest's entry for one source.
fn source<'a>(manifest: &'a Value, source_id: &str) -> &'a Value {
    manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["sourceId"] == source_id)
        .unwrap_or_else(|| panic!("{source_id} is not in the manifest"))
}

#[test]
fn a_task_uses_the_skill_it_names_and_leaves_the_rest_in_the_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let project_root = directory.path().join("project");
    std::fs::create_dir_all(project_root.join(".tramontana").join("skills")).unwrap();
    std::fs::write(
        project_root.join(".tramontana").join("skills").join("review.md"),
        "---\nid: review\nname: Review Rust\ndescription: Look for unsafe code.\n---\nRead every changed Rust file for unsafe blocks.\n",
    )
    .unwrap();
    std::fs::write(
        project_root
            .join(".tramontana")
            .join("skills")
            .join("release.md"),
        "---\nid: release\nname: Release checklist\n---\nCheck the notes before tagging.\n",
    )
    .unwrap();
    let profile = directory.path().join("profiles").join("skills.sqlite");

    let mut open = vec![text("project"), text("open"), project_root.into_os_string()];
    open.extend(profile_argument(&profile));
    let opened: Value = serde_json::from_slice(&run(open).stdout).unwrap();
    let project_id = opened["project"]["projectId"].as_str().unwrap().to_owned();
    // The description is kept, so a person choosing has something to read.
    let described = opened["configuration"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["declaredId"] == "review")
        .unwrap();
    assert_eq!(described["declaredDescription"], "Look for unsafe code.");

    let mut conversation = vec![
        text("conversation"),
        text("create"),
        text("Skills"),
        text("--project-id"),
        text(&project_id),
    ];
    conversation.extend(profile_argument(&profile));
    let created: Value = serde_json::from_slice(&run(conversation).stdout).unwrap();
    let conversation_id = created["conversationId"].as_str().unwrap().to_owned();

    let named = manifest_of_task(&profile, &conversation_id, Some("review"));
    let review = source(&named, ".tramontana/skills/review.md");
    assert_eq!(review["included"], true, "the named skill's text was sent");
    assert_eq!(
        source(&named, ".tramontana/skills/release.md")["exclusionReason"],
        "context.notActive",
        "a skill nobody named stays in the inventory"
    );

    // A second task in the same conversation names none, and gets none: the
    // selection belongs to the task, not to the project.
    let unnamed = manifest_of_task(&profile, &conversation_id, None);
    assert_eq!(
        source(&unnamed, ".tramontana/skills/review.md")["exclusionReason"],
        "context.notActive",
        "an earlier task's choice must not carry into the next one"
    );
}
