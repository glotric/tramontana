//! Managing credentials in the operating system's own store.
//!
//! `SECURITY_AND_PERMISSIONS.md` §7. Three things this command does that are
//! not incidental:
//!
//! - **The value is never a process argument.** `--value` does not exist. A
//!   command line is visible to every other process on the machine and lands
//!   in shell history; a credential passed that way is already disclosed. It
//!   is read from standard input.
//! - **Nothing is echoed.** Not on set, not on show, not in an error. `status`
//!   answers whether a credential is configured, which is the question an
//!   interface actually needs, and it answers it without resolving the value.
//! - **A locked store and an empty one are told apart.** A user whose Keychain
//!   is locked must not be told their credential is missing.

use std::io::Read;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use tramontana_core::contracts::{SecretRef, SecretStore};
use tramontana_core::ports::SecretVault;
use tramontana_platform::PlatformSecretVault;

/// Arguments for `tramontana secret`.
#[derive(Debug, Args)]
pub(crate) struct SecretArgs {
    #[command(subcommand)]
    command: SecretCommand,
}

#[derive(Debug, Subcommand)]
enum SecretCommand {
    /// Store a credential, read from standard input.
    ///
    /// The value is never accepted as an argument. Pipe it in:
    /// `printf %s "$KEY" | tramontana secret set openai.apiKey`
    Set(KeyArgs),
    /// Report whether a credential is stored, without reading its value.
    Status(KeyArgs),
    /// Remove a credential from the operating system store.
    ///
    /// Uninstalling Tramontana does not do this; this command does.
    Forget(KeyArgs),
}

#[derive(Debug, Args)]
struct KeyArgs {
    /// Opaque reference the credential is filed under, such as `openai.apiKey`.
    #[arg(value_name = "KEY")]
    key: String,
}

/// Run a `secret` subcommand.
pub(crate) async fn execute(args: SecretArgs) -> ExitCode {
    let vault = PlatformSecretVault::new();
    match args.command {
        SecretCommand::Set(args) => set(&vault, &args.key).await,
        SecretCommand::Status(args) => status(&vault, &args.key).await,
        SecretCommand::Forget(args) => forget(&vault, &args.key).await,
    }
}

/// The opaque reference for `key`.
fn reference(key: &str) -> SecretRef {
    SecretRef {
        key: key.to_owned(),
        store: SecretStore::Platform,
    }
}

async fn set(vault: &PlatformSecretVault, key: &str) -> ExitCode {
    let mut value = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut value) {
        eprintln!("error: could not read the credential from standard input: {error}");
        return ExitCode::FAILURE;
    }
    // A trailing newline from `echo` or a heredoc is almost never part of the
    // credential, and a key with an invisible newline on the end fails to
    // authenticate for a reason nobody can see.
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        eprintln!("error: nothing was read from standard input; the credential was not stored");
        return ExitCode::FAILURE;
    }

    match vault.store(&reference(key), value).await {
        // The length, not the value. Enough to notice a truncated paste.
        Ok(()) => {
            println!(
                "stored {key} ({} characters) in the platform store",
                value.len()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            ExitCode::FAILURE
        }
    }
}

async fn status(vault: &PlatformSecretVault, key: &str) -> ExitCode {
    match vault.contains(&reference(key)).await {
        Ok(true) => {
            println!("{key}: configured");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("{key}: not configured");
            ExitCode::SUCCESS
        }
        // Not "not configured". The store could not answer, and saying it was
        // empty would send the user to re-enter a key that is already there.
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            ExitCode::FAILURE
        }
    }
}

async fn forget(vault: &PlatformSecretVault, key: &str) -> ExitCode {
    match vault.delete(&reference(key)).await {
        Ok(true) => {
            println!("{key}: removed from the platform store");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("{key}: nothing was stored");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {:?}: {}", error.code, error.message);
            ExitCode::FAILURE
        }
    }
}
