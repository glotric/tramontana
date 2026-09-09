//! Headless adapter over the Tramontana application services.
//!
//! A development, test, and recovery surface — a peer of the desktop shell over
//! the same Rust services, never a second implementation of them
//! (`AGENTS.md`, "Architectural boundaries"). Every command here translates
//! arguments into an application call and renders the result; no routing,
//! permission, or task behavior lives in this crate.

mod approval;
mod bundle;
mod configuration;
mod conversation;
mod project;
mod render;
mod run;
mod secret;
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
    /// Open, list, and inspect durable projects.
    Project(project::ProjectArgs),
    /// Create, list, and inspect durable conversations.
    Conversation(conversation::ConversationArgs),
    /// Export and import portable bundles.
    Bundle(bundle::BundleArgs),
    /// Resolve and inspect project-authored configuration.
    Configuration(configuration::ConfigurationArgs),
    /// Store, inspect, and remove credentials in the operating system store.
    Secret(secret::SecretArgs),
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
        Some(Command::Info) | None => {
            print_info();
            std::process::ExitCode::SUCCESS
        }
        Some(command) => {
            // The CLI owns the runtime; the services are runtime-agnostic.
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("error: could not start the async runtime: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            runtime.block_on(execute(command))
        }
    }
}

async fn execute(command: Command) -> std::process::ExitCode {
    match command {
        Command::Project(args) => project::execute(args).await,
        Command::Conversation(args) => conversation::execute(args).await,
        Command::Bundle(args) => bundle::execute(args).await,
        Command::Configuration(args) => configuration::execute(args).await,
        Command::Secret(args) => secret::execute(args).await,
        Command::Task(args) => run::execute(args).await,
        Command::Info => std::process::ExitCode::SUCCESS,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_cannot_be_passed_as_a_process_argument() {
        // A command line is visible to every other process on the machine and
        // lands in shell history. There is no flag that takes a value, and
        // this asserts it rather than trusting that nobody adds one.
        assert!(Cli::try_parse_from(["tramontana", "secret", "set", "openai.apiKey"]).is_ok());
        for rejected in [
            vec![
                "tramontana",
                "secret",
                "set",
                "openai.apiKey",
                "--value",
                "sk-x",
            ],
            vec!["tramontana", "secret", "set", "openai.apiKey", "sk-x"],
            vec![
                "tramontana",
                "secret",
                "set",
                "openai.apiKey",
                "--password",
                "sk-x",
            ],
        ] {
            assert!(
                Cli::try_parse_from(&rejected).is_err(),
                "the CLI accepted a credential as an argument: {rejected:?}"
            );
        }
    }

    #[test]
    fn m2_management_commands_parse() {
        assert!(Cli::try_parse_from(["tramontana", "project", "open", "."]).is_ok());
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "configuration",
                "inspect",
                "project-1",
                "--active-path",
                "src/lib.rs",
                "--skill-id",
                "rust",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "conversation",
                "create",
                "M2 demo",
                "--project-id",
                "project-1",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "task",
                "replay",
                "task-1",
                "--after-sequence",
                "4",
            ])
            .is_ok()
        );
    }

    #[test]
    fn m4_bundle_and_checkpoint_commands_parse() {
        assert!(Cli::try_parse_from(["tramontana", "bundle", "export", "./out"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "bundle",
                "export",
                "./out",
                "--task-id",
                "task-1",
                "--include-absolute-paths",
            ])
            .is_ok()
        );
        // A bundle is either a project or a task, never both at once.
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "bundle",
                "export",
                "./out",
                "--project-id",
                "project-1",
                "--task-id",
                "task-1",
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["tramontana", "bundle", "inspect", "./out"]).is_ok());
        assert!(Cli::try_parse_from(["tramontana", "bundle", "import", "./out"]).is_ok());
        assert!(Cli::try_parse_from(["tramontana", "task", "inspect", "task-1"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "task",
                "resume",
                "task-1",
                "--checkpoint-id",
                "checkpoint-1",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["tramontana", "task", "handoff", "task-1"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "task",
                "handoff",
                "task-1",
                "--checkpoint-id",
                "checkpoint-1",
                "--to",
                "fake.receiving",
            ])
            .is_ok()
        );
    }

    #[test]
    fn the_m1_task_invocation_remains_compatible() {
        assert!(
            Cli::try_parse_from([
                "tramontana",
                "task",
                "Say hello",
                "--chunk-delay-ms",
                "0",
                "--json",
            ])
            .is_ok()
        );
    }
}
