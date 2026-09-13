//! Real CLI, SQLite, HTTP adapter and filesystem tools; synthetic inference only.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "isolated integration fixture assertions"
)]
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};

fn command(profile: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tramontana"))
        .args(args)
        .arg("--profile")
        .arg(profile)
        .output()
        .unwrap()
}
fn run(profile: &Path, args: &[&str]) -> Output {
    let out = command(profile, args);
    assert!(
        out.status.success(),
        "{:?}\n{}\n{}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn json_out(profile: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&run(profile, args).stdout).unwrap()
}

#[test]
fn configured_reference_worker_reads_a_repository_and_reopens_its_checkpoint() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository ü");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("README.md"),
        "Fixture build command: cargo test --workspace\n",
    )
    .unwrap();
    let profile = temp.path().join("reference.sqlite");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    run(
        &profile,
        &[
            "provider",
            "add-openai",
            "fixture",
            "--base-url",
            &base,
            "--model",
            "synthetic",
        ],
    );
    let project = json_out(&profile, &["project", "open", root.to_str().unwrap()]);
    let project_id = project["project"]["projectId"].as_str().unwrap();
    let conversation = json_out(
        &profile,
        &[
            "conversation",
            "create",
            "Question",
            "--project-id",
            project_id,
        ],
    );
    let conversation_id = conversation["conversationId"].as_str().unwrap();
    let task_args = [
        "task",
        "What is the build command in README.md?",
        "--conversation-id",
        conversation_id,
        "--json",
    ];
    let unsupported = command(&profile, &task_args);
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stdout).contains("capabilityUnsupported"));
    assert!(matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock));
    run(&profile, &["provider", "tool-calls", "fixture"]);

    let server = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for turn in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "no model request arrived");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            // On macOS an accepted socket inherits the listener's non-blocking
            // mode, so without this `read` can return `WouldBlock` before the
            // CLI's request has arrived — a load-dependent failure that took
            // down a full local check. The listener itself stays non-blocking:
            // the assertion before this thread relies on it to prove that no
            // request was made while the capability was unknown.
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&chunk[..read]);
                assert!(bytes.len() < 2_000_000);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let len: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + len {
                        break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + len])
                            .unwrap();
                    }
                }
            };
            let delta = if turn == 0 {
                let read_tool = body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|tool| {
                        tool["function"]["description"]
                            .as_str()
                            .unwrap()
                            .starts_with("Read a bounded")
                    })
                    .unwrap();
                json!({"tool_calls":[{"index":0,"id":"call-synthetic","type":"function","function":{"name":read_tool["function"]["name"],"arguments":"{\"path\":\"README.md\"}"}}]})
            } else {
                let observation = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|message| message["role"] == "tool")
                    .unwrap();
                assert!(
                    observation["content"]
                        .as_str()
                        .unwrap()
                        .contains("cargo test --workspace")
                );
                json!({"content":"README.md says to run cargo test --workspace."})
            };
            let payload = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":delta,"finish_reason":null}]}),
                json!({"choices":[{"delta":{},"finish_reason":if turn==0 {"tool_calls"} else {"stop"}}]})
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            socket.write_all(response.as_bytes()).unwrap();
            bodies.push(body);
        }
        bodies
    });
    let output = run(&profile, &task_args);
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.last().unwrap()["kind"], "taskCompleted");
    assert!(
        events.last().unwrap()["payload"]["summary"]
            .as_str()
            .unwrap()
            .contains("cargo test --workspace")
    );
    assert!(
        events
            .iter()
            .any(|event| event["payload"]["descriptor"]["agentId"] == "builtin.reference")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["kind"] == "toolCompleted")
            .count(),
        1
    );
    let task = events[0]["taskId"].as_str().unwrap();
    let checkpoint = json_out(&profile, &["task", "checkpoint", task]);
    assert_eq!(checkpoint["completedStepIds"], json!(["reference-1"]));
    assert_eq!(checkpoint["schemaVersion"], 1);
    let inspected = json_out(&profile, &["task", "inspect", task]);
    assert_eq!(inspected["checkpoint"], checkpoint);
    let replay = json_out(&profile, &["task", "replay", task]);
    assert_eq!(replay.as_array().unwrap()[..events.len()], events);
    assert_eq!(server.join().unwrap().len(), 2);
}
