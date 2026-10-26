//! What the CLI's and the desktop's end-to-end tests both need: Git that
//! ignores whoever runs it, and the wire side of a scripted
//! OpenAI-compatible model. Test-only: never a dependency of a
//! shipped crate.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test support: a panic is the failure report"
)]

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

/// Run Git in `root`, unaffected by whoever runs the test, and return its
/// output.
///
/// # Panics
///
/// When Git fails.
pub fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The JSON body of one HTTP request.
///
/// # Panics
///
/// When the request ends early, is not JSON, or passes any sane size.
pub fn read_body(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let read = socket.read(&mut chunk).unwrap();
        assert!(read > 0, "the request ended before its body");
        bytes.extend_from_slice(&chunk[..read]);
        assert!(bytes.len() < 2_000_000, "a request past any sane size");
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

/// Answer one request with a streamed turn: `delta`, then `finish`.
///
/// # Errors
///
/// When the client has gone.
pub fn reply(socket: &mut TcpStream, delta: &Value, finish: &str) -> std::io::Result<()> {
    let payload = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"delta":delta,"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":finish}]})
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    socket.write_all(response.as_bytes())
}

/// A scripted call to the tool whose description starts with `description`.
/// Names on the wire are provider-neutral aliases, so a tool is found by what
/// it says it does.
///
/// # Panics
///
/// When no such tool was offered.
#[must_use]
pub fn call(
    turn: usize,
    body: &Value,
    description: &str,
    arguments: Value,
) -> (Value, &'static str) {
    let name = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| {
            tool["function"]["description"]
                .as_str()
                .unwrap()
                .starts_with(description)
        })
        .unwrap_or_else(|| panic!("no tool described as {description:?} was offered"))["function"]
        ["name"]
        .clone();
    (
        json!({ "tool_calls": [{
            "index": 0,
            "id": format!("call-{turn}"),
            "type": "function",
            "function": { "name": name, "arguments": arguments.to_string() },
        }] }),
        "tool_calls",
    )
}

/// A scripted final answer.
#[must_use]
pub fn answer(text: &str) -> (Value, &'static str) {
    (json!({ "content": text }), "stop")
}

/// The latest tool result the model was shown.
///
/// # Panics
///
/// When it was shown none.
#[must_use]
pub fn observation(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The content hash in the latest read the model was shown, which its patch
/// must carry. The adapter wraps the brokered result as `{content, truncated}`.
///
/// # Panics
///
/// When the latest result was not a read.
#[must_use]
pub fn read_hash(body: &Value) -> String {
    let seen = observation(body);
    let read: Value = serde_json::from_str(&seen).unwrap();
    read["content"]["output"]["contentHash"]
        .as_str()
        .unwrap_or_else(|| panic!("no contentHash in the read the model saw: {seen}"))
        .to_owned()
}
