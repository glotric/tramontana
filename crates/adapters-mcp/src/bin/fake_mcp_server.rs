//! A stand-in MCP server over stdio, for this crate's tests.
//!
//! It speaks JSON-RPC by hand rather than through an SDK, so it can misbehave
//! in ways a well-built server would not: a line that is not JSON, a reply of
//! the wrong shape, a message larger than any limit, tools whose names or
//! schemas are hostile, pages that never end, a crash or a hang mid-call.
//!
//! `tramontana-fake-mcp-server <mode> [pid-file]`. The modes are listed in
//! `main`. `spawn-child` writes the pid of a child it starts to `pid-file`,
//! so a test can check that stopping the server stopped the child too.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test fixture; a panic is a visible test failure"
)]

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn send(out: &mut impl Write, message: &Value) {
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn reply(out: &mut impl Write, id: &Value, result: Value) {
    send(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

/// The tools a well-behaved server offers.
fn ordinary_tools() -> Value {
    json!([
        {
            "name": "greeting",
            "title": "Greeting",
            "description": "Says hello to someone.",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "inspect",
            "description": "Reports the working directory and the environment variable names it was given.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "stamp",
            "description": "Claims nothing about what it does.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

/// Names and schemas a server should not be trusted with.
fn hostile_tools() -> Value {
    let long_name = "n".repeat(300);
    let long_description = "d".repeat(100_000);
    json!([
        {"name": "ok_tool", "description": "The one acceptable tool.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "Search", "description": "Collides with `search` once lowercased.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "search", "description": "Collides with `Search` once lowercased.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "../escape", "description": "Tries to leave its namespace.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": long_name, "description": "A name longer than any limit.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "rambling", "description": long_description,
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "referencing", "description": "A schema with a reference.",
         "inputSchema": {"type": "object", "properties": {"a": {"$ref": "#/definitions/x"}}}},
        {"name": "not_an_object", "description": "Arguments that are not an object.",
         "inputSchema": {"type": "string"}}
    ])
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next().unwrap_or_else(|| "ok".to_owned());
    let pid_file = arguments.next();

    if mode == "spawn-child" {
        #[allow(
            clippy::zombie_processes,
            reason = "left running on purpose: the test proves stopping the App stops it"
        )]
        let child = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("start a child");
        if let Some(path) = &pid_file {
            std::fs::write(path, child.id().to_string()).expect("write the child's pid");
        }
    }
    if mode == "stderr-flood" {
        std::thread::spawn(|| {
            let line = "noise ".repeat(100);
            loop {
                eprintln!("{line}");
            }
        });
    }

    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        if mode == "malformed" && !id.is_null() {
            let _ = writeln!(out, "this line is not JSON");
        }
        match method {
            "initialize" => {
                if mode == "silent-start" {
                    continue;
                }
                let version = message
                    .pointer("/params/protocolVersion")
                    .cloned()
                    .unwrap_or(Value::Null);
                reply(
                    &mut out,
                    &id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fake-mcp", "version": "0.0.0"}
                    }),
                );
            }
            "tools/list" => match mode.as_str() {
                "hostile-tools" => reply(&mut out, &id, json!({"tools": hostile_tools()})),
                "endless-pages" => reply(
                    &mut out,
                    &id,
                    json!({"tools": ordinary_tools(), "nextCursor": "again"}),
                ),
                _ => reply(&mut out, &id, json!({"tools": ordinary_tools()})),
            },
            "tools/call" => {
                let name = message
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match mode.as_str() {
                    "crash-on-call" => std::process::exit(3),
                    "hang-on-call" => {}
                    "oversized-result" => {
                        let text = "x".repeat(4 * 1024 * 1024);
                        reply(
                            &mut out,
                            &id,
                            json!({"content": [{"type": "text", "text": text}]}),
                        );
                    }
                    "large-text" => {
                        let text = "y".repeat(512 * 1024);
                        reply(
                            &mut out,
                            &id,
                            json!({"content": [{"type": "text", "text": text}]}),
                        );
                    }
                    "error-result" => reply(
                        &mut out,
                        &id,
                        json!({"content": [{"type": "text", "text": "the App failed"}], "isError": true}),
                    ),
                    "wrong-shape" => reply(&mut out, &id, json!(42)),
                    _ if name == "inspect" => {
                        let mut names: Vec<String> = std::env::vars().map(|(key, _)| key).collect();
                        names.sort();
                        let cwd = std::env::current_dir()
                            .map(|path| path.display().to_string())
                            .unwrap_or_default();
                        reply(
                            &mut out,
                            &id,
                            json!({"content": [{"type": "text", "text": json!({"cwd": cwd, "env": names}).to_string()}]}),
                        );
                    }
                    _ => {
                        let who = message
                            .pointer("/params/arguments/name")
                            .and_then(Value::as_str)
                            .unwrap_or("world");
                        reply(
                            &mut out,
                            &id,
                            json!({"content": [{"type": "text", "text": format!("Hello, {who}")}]}),
                        );
                    }
                }
            }
            "notifications/cancelled" => eprintln!("cancelled: {line}"),
            _ if !id.is_null() => send(
                &mut out,
                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such method"}}),
            ),
            _ => {}
        }
    }
}
