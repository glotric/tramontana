//! Headless adapter over the Tramontana application services.
//!
//! A development, test, and recovery surface — a peer of the desktop shell over
//! the same Rust services, never a second implementation of them
//! (`AGENTS.md`, "Architectural boundaries"). Every command here translates
//! arguments into an application call and renders the result; no routing,
//! permission, or task behavior lives in this crate.

mod render;
mod run;
mod wiring;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "tramontana",
    version,
    about = "Headless adapter for the Tramontana control plane",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print version and contract-schema information as JSON.
    Info,
    /// Run one task against the fake providers and stream its activity.
    ///
    /// M1's walking skeleton: the same application service the desktop shell
    /// calls, driven from a terminal so the event contract can be inspected
    /// without a window.
    Task(run::TaskArgs),
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Task(args)) => run::execute(args),
        Some(Command::Info) | None => {
            print_info();
            std::process::ExitCode::SUCCESS
        }
    }
}

/// Report what this binary is and which contract schema version it speaks.
///
/// Emitted as JSON so `bootstrap` and CI can assert on it without parsing prose.
fn print_info() {
    println!(
        "{{\"name\":\"tramontana-cli\",\"version\":\"{}\",\"schemaVersion\":{}}}",
        env!("CARGO_PKG_VERSION"),
        tramontana_core::SCHEMA_VERSION,
    );
}
