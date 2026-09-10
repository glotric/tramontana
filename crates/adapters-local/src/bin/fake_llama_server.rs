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

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
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
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) else {
        eprintln!("error: could not bind 127.0.0.1:{port}");
        std::process::exit(3);
    };

    let started = Instant::now();
    println!("HTTP server listening on 127.0.0.1:{port}");
    let _ = std::io::stdout().flush();

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };

        // Read just the request line; nothing here needs the rest.
        let mut request_line = String::new();
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(clone) => clone,
            Err(_) => continue,
        });
        if reader.read_line(&mut request_line).is_err() {
            continue;
        }

        let loading = mode == "never-ready"
            || (mode == "slow" && started.elapsed() < Duration::from_millis(load_ms));
        let (status, body) = if !request_line.contains("/health") {
            ("404 Not Found", "{}".to_owned())
        } else if loading {
            (
                "503 Service Unavailable",
                r#"{"status":"loading model"}"#.to_owned(),
            )
        } else {
            ("200 OK", r#"{"status":"ok"}"#.to_owned())
        };

        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

/// The value following `name` in the argument list.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}
