//! A stand-in for `llama-server`, so supervision can be tested for real.
//!
//! Not a mock of the supervisor's own calls: a real process, with a real
//! port, real stdout, and a real exit status. The behaviours that matter for
//! supervision are the failure ones — a binary that refuses its model, one
//! that starts and never becomes ready, one that ignores a polite stop — and
//! none of them can be exercised without something that actually does them.
//!
//! A test fixture. The desktop application packages the Tauri binary and
//! nothing from here.
//!
//! Behaviour is chosen with `--fake-mode`:
//!
//! - unset or `ready`   — serve `/health` as 200 after a short load
//! - `slow`             — answer 503 for `TRAMONTANA_FAKE_LOAD_MS`, then 200
//! - `never-ready`      — answer 503 forever
//! - `exit-immediately` — print a plausible error and exit non-zero
//! - `spawn-child`      — serve, having spawned a long-lived child of its own,
//!   so that "the whole process group is cleaned up" can be tested rather than
//!   asserted
//!
//! Chat completions, chosen with `--fake-chat`, so the model provider contract
//! can run against a supervised runtime (`BUILD_ORDER.md` §17):
//!
//! - `streams` — each `--fake-chunk` value as a content delta, then a clean
//!   finish, in the OpenAI-compatible shape `llama-server` speaks
//! - `fails`   — 503 with no body
//! - `silent`  — headers, then nothing, so a cancellation lands mid-stream
//!
//! Without it, anything but `/health` is 404.
//!
//! Like `llama-server`, it takes an API key from `LLAMA_API_KEY` and then
//! refuses any request but `/health` that does not carry it as a bearer
//! token, with 401. Checked against the real runtime, build 10964.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// The build this fixture claims to be.
const BUILD: u64 = 9999;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version") {
        // The banner shape the supervisor parses, on stdout.
        println!("version: {BUILD} (fake0000)");
        println!("built with tramontana test fixture");
        return;
    }

    // Read from the argument list, not the environment. The supervisor clears
    // the environment before spawning — that is the behaviour §7 asks for —
    // so a fixture configured through it would be configuring nothing, and
    // every case would silently run in the default mode.
    //
    // `TRAMONTANA_FAKE_MODE` still works for the helper this process spawns
    // itself, where there is no supervisor in between.
    let mode = flag(&args, "--fake-mode")
        .or_else(|| std::env::var("TRAMONTANA_FAKE_MODE").ok())
        .unwrap_or_else(|| "ready".to_owned());
    let load_ms: u64 = flag(&args, "--fake-load-ms")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);

    let port = flag(&args, "--port").and_then(|value| value.parse::<u16>().ok());
    let model = flag(&args, "--model");

    // Printed so a test can assert the supervisor passed what it claims to.
    println!("fake llama-server starting: model={model:?} port={port:?} mode={mode}");
    let _ = std::io::stdout().flush();

    if mode == "exit-immediately" {
        eprintln!(
            "error: failed to load model from {}",
            model.unwrap_or_default()
        );
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }

    // A grandchild in the same process group. Killing only the direct child
    // would leave this running, which is the failure the group is for.
    let mut helper = None;
    if mode == "spawn-child" {
        let Ok(self_path) = std::env::current_exe() else {
            eprintln!("error: could not find my own path");
            std::process::exit(4);
        };
        helper = std::process::Command::new(self_path)
            .env("TRAMONTANA_FAKE_MODE", "idle-child")
            .spawn()
            .ok();
        if let Some(child) = helper.as_ref() {
            println!("spawned helper pid={}", child.id());
            let _ = std::io::stdout().flush();
        }
    }
    if mode == "idle-child" {
        // Sleeps until something signals the group.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    drop(helper);

    let Some(port) = port else {
        eprintln!("error: no --port given");
        std::process::exit(2);
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            // The OS error, not only the fact of failure. "could not bind"
            // alone made a port collision indistinguishable from any other
            // bind problem, which is how a wrong diagnosis got as far as a
            // fix before the evidence disagreed with it.
            eprintln!("error: could not bind 127.0.0.1:{port}: {error}");
            std::process::exit(3);
        }
    };

    let chat = flag(&args, "--fake-chat").unwrap_or_default();
    let key = std::env::var("LLAMA_API_KEY")
        .ok()
        .filter(|key| !key.is_empty());
    let chunks = flags(&args, "--fake-chunk");
    let started = Instant::now();
    println!("HTTP server listening on 127.0.0.1:{port}");
    let _ = std::io::stdout().flush();

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let loading = mode == "never-ready"
            || (mode == "slow" && started.elapsed() < Duration::from_millis(load_ms));
        // One thread per connection, so a stream `silent` holds open does not
        // stop `/health` from answering.
        let chat = chat.clone();
        let chunks = chunks.clone();
        let key = key.clone();
        std::thread::spawn(move || serve(stream, loading, &chat, &chunks, key.as_deref()));
    }
}

/// Answer one connection.
fn serve(mut stream: TcpStream, loading: bool, chat: &str, chunks: &[String], key: Option<&str>) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // The rest of the request too, so a client still sending its body is not
    // answered with a reset.
    let mut length: usize = 0;
    let mut bearer = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" => break,
            Ok(_) => {
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    bearer = value.trim().strip_prefix("Bearer ").map(str::to_owned);
                }
            }
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);

    if request_line.contains("/health") {
        if loading {
            respond(
                &mut stream,
                "503 Service Unavailable",
                r#"{"status":"loading model"}"#,
            );
        } else {
            respond(&mut stream, "200 OK", r#"{"status":"ok"}"#);
        }
    } else if key.is_some() && bearer.as_deref() != key {
        respond(
            &mut stream,
            "401 Unauthorized",
            r#"{"error":{"message":"Invalid API Key"}}"#,
        );
    } else if request_line.contains("/chat/completions") {
        chat_completion(&mut stream, chat, chunks);
    } else {
        respond(&mut stream, "404 Not Found", "{}");
    }
}

/// A chat completion, as `--fake-chat` says.
fn chat_completion(stream: &mut TcpStream, chat: &str, chunks: &[String]) {
    match chat {
        "streams" | "silent" => {
            let head =
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            if stream.write_all(head.as_bytes()).is_err() {
                return;
            }
            let _ = stream.flush();
            if chat == "silent" {
                // Held open and never written to: the client's cancellation
                // or deadline has to be what ends it.
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            for chunk in chunks {
                let frame = serde_json::json!({ "choices": [{ "delta": { "content": chunk } }] });
                if stream
                    .write_all(format!("data: {frame}\n\n").as_bytes())
                    .is_err()
                {
                    return;
                }
                let _ = stream.flush();
            }
            let finish = serde_json::json!({
                "choices": [{ "delta": {}, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 11, "completion_tokens": 3 }
            });
            let _ = stream.write_all(format!("data: {finish}\n\ndata: [DONE]\n\n").as_bytes());
            let _ = stream.flush();
        }
        "fails" => respond(stream, "503 Service Unavailable", ""),
        _ => respond(stream, "404 Not Found", "{}"),
    }
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Every value following `name` in the argument list.
fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .collect()
}

/// The value following `name` in the argument list.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}
