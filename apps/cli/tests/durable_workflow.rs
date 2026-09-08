//! Cross-process proof for the M2 headless workflow.

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

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be one JSON value")
}

#[test]
fn separate_cli_processes_reopen_the_same_complete_record() {
    let directory = tempfile::tempdir().unwrap();
    let project_root = directory.path().join("project with ünicode");
    std::fs::create_dir_all(&project_root).unwrap();
    let execution_marker = project_root.join("markdown-was-executed");
    std::fs::write(
        project_root.join("AGENTS.md"),
        format!(
            "---\nenabled: true\n---\nThis is configuration data. Never run: touch {}\n",
            execution_marker.display()
        ),
    )
    .unwrap();
    let profile = directory.path().join("profiles").join("m2.sqlite");

    let mut open_args = vec![text("project"), text("open"), project_root.into_os_string()];
    open_args.extend(profile_argument(&profile));
    let opened = json(&run(open_args));
    let project_id = opened["project"]["projectId"].as_str().unwrap().to_owned();
    let manifest_hash = opened["configuration"]["manifestHash"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        opened["configuration"]["sources"].as_array().unwrap().len(),
        1
    );
    assert_eq!(opened["configuration"]["sources"][0]["active"], true);

    let mut conversation_args = vec![
        text("conversation"),
        text("create"),
        text("Durable conversation"),
        text("--project-id"),
        text(&project_id),
    ];
    conversation_args.extend(profile_argument(&profile));
    let conversation = json(&run(conversation_args));
    let conversation_id = conversation["conversationId"].as_str().unwrap().to_owned();

    let mut task_args = vec![
        text("task"),
        text("Survive separate processes"),
        text("--conversation-id"),
        text(&conversation_id),
        text("--chunk-delay-ms"),
        text("0"),
        text("--json"),
    ];
    task_args.extend(profile_argument(&profile));
    let task_output = run(task_args);
    let streamed_events: Vec<Value> = String::from_utf8(task_output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let streamed_sequences: Vec<u64> = streamed_events
        .iter()
        .map(|event| event["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(
        streamed_sequences,
        (1..=u64::try_from(streamed_events.len()).unwrap()).collect::<Vec<_>>()
    );
    assert_eq!(streamed_events.last().unwrap()["kind"], "taskCompleted");

    let mut list_args = vec![
        text("task"),
        text("list"),
        text("--conversation-id"),
        text(&conversation_id),
    ];
    list_args.extend(profile_argument(&profile));
    let tasks = json(&run(list_args));
    let task = &tasks.as_array().unwrap()[0];
    let task_id = task["taskId"].as_str().unwrap().to_owned();
    assert_eq!(task["state"], "completed");
    assert_eq!(task["configurationManifestHash"], manifest_hash);
    assert_eq!(task["lastSequence"], streamed_events.len());

    let mut replay_args = vec![text("task"), text("replay"), text(&task_id)];
    replay_args.extend(profile_argument(&profile));
    assert_eq!(json(&run(replay_args)), Value::Array(streamed_events));

    let mut inspect_args = vec![text("project"), text("inspect"), text(&project_id)];
    inspect_args.extend(profile_argument(&profile));
    let inspected = json(&run(inspect_args));
    assert_eq!(inspected["configuration"]["manifestHash"], manifest_hash);
    assert!(
        !execution_marker.exists(),
        "loading Markdown must never execute its body"
    );
}
