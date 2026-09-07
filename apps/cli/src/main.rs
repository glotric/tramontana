//! Headless adapter over the Tramontana application services.
//!
//! This is a development, test, and recovery surface — a peer of the desktop
//! shell over the same Rust services, never a second implementation of them
//! (`AGENTS.md`, "Architectural boundaries"). Any behavior it appears to own
//! belongs in `tramontana-core`.
//!
//! M0 provides only version reporting. Project, task, event, and configuration
//! commands arrive with the milestones that implement them.

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
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Info) | None => print_info(),
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
