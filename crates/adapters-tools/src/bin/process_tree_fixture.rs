//! Real processes doing what commands do, for testing the command broker.
//!
//! Process-tree ownership cannot be tested with mocks: the claim is about
//! grandchildren that sleep, leaders that exit and leave children behind, and
//! pipes that fill up. This starts them for real.
//!
//! A test fixture. The desktop application packages the Tauri binary and
//! nothing from here.
//!
//! Behaviour is chosen by the first argument:
//!
//! - `tree DIR DEPTH FANOUT` — start FANOUT copies of itself one level
//!   shallower, record this process's id as a file in DIR, then sleep; a leaf
//!   (depth 0) only records and sleeps
//! - `orphan DIR` — start one sleeping child, wait until it has recorded
//!   itself, record this process, and exit at once, leaving the child behind
//! - `args ...` — print each remaining argument on its own line
//! - `env` — print every environment variable as `NAME=VALUE`
//! - `pwd` — print the working directory
//! - `flood STDOUT STDERR` — write that many bytes to each stream
//! - `stdin` — read standard input to its end and print how many bytes came
//! - `exit CODE` — exit with that code

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test fixture; a panic is its failure report"
)]

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("tree") => tree(
            Path::new(&args[1]),
            args[2].parse().unwrap(),
            args[3].parse().unwrap(),
        ),
        Some("orphan") => orphan(Path::new(&args[1])),
        Some("args") => {
            for argument in &args[1..] {
                println!("{argument}");
            }
        }
        Some("env") => {
            for (name, value) in std::env::vars_os() {
                println!("{}={}", name.to_string_lossy(), value.to_string_lossy());
            }
        }
        Some("pwd") => println!("{}", std::env::current_dir().unwrap().display()),
        Some("flood") => {
            let out = vec![b'o'; args[1].parse().unwrap()];
            let err = vec![b'e'; args[2].parse().unwrap()];
            std::io::stdout().write_all(&out).unwrap();
            std::io::stderr().write_all(&err).unwrap();
        }
        Some("stdin") => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap();
            println!("{}", bytes.len());
        }
        Some("exit") => std::process::exit(args[1].parse().unwrap()),
        other => panic!("unknown mode {other:?}"),
    }
}

/// Record this process, so a test can check later that it is gone.
fn record(dir: &Path) {
    std::fs::write(dir.join(std::process::id().to_string()), "").unwrap();
}

fn copy_of_self(arguments: &[&str]) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(arguments)
        .spawn()
        .unwrap()
}

fn tree(dir: &Path, depth: u32, fanout: u32) {
    let mut children = Vec::new();
    if depth > 0 {
        let dir = dir.to_string_lossy();
        let depth = (depth - 1).to_string();
        let fanout_text = fanout.to_string();
        for _ in 0..fanout {
            children.push(copy_of_self(&["tree", &dir, &depth, &fanout_text]));
        }
    }
    record(dir);
    // Far longer than any deadline a test sets, and short enough that what a
    // failed run leaves behind clears itself within minutes.
    std::thread::sleep(Duration::from_secs(120));
    drop(children);
}

fn orphan(dir: &Path) {
    #[allow(
        clippy::zombie_processes,
        reason = "the point of this mode: exit without waiting, leaving the child to the broker"
    )]
    let _child = copy_of_self(&["tree", &dir.to_string_lossy(), "0", "0"]);
    let started = Instant::now();
    while std::fs::read_dir(dir).unwrap().count() == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "child never started"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    record(dir);
}
