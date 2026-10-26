//! `tramontana-server`: the host program (`SHARED_PROJECTS_PLAN.md` SP1).
//!
//! The same application service as the desktop, opened over a profile in a
//! data folder and served to paired desktops (ADRs 0074-0076). A thin
//! adapter, like `apps/cli`: no policy lives here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use tramontana_core::ports::{Approver, SecretResolver};
use tramontana_host::{Devices, Host, RemoteApprover};
use tramontana_platform::{FileSecretVault, PlatformSecretVault};

#[derive(Parser)]
#[command(
    name = "tramontana-server",
    version,
    about = "Tramontana's host program"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve paired desktops.
    Serve {
        /// The folder holding the host's profile and device list.
        #[arg(long)]
        data: PathBuf,
        /// Where to listen, such as 0.0.0.0:7443.
        #[arg(long, default_value = "0.0.0.0:7443")]
        listen: String,
        /// The address desktops use to reach this host, for invite links.
        #[arg(long)]
        public_address: Option<String>,
        /// The host's certificate, PEM.
        #[arg(long)]
        cert: PathBuf,
        /// The certificate's private key, PEM, PKCS#8.
        #[arg(long)]
        key: PathBuf,
        /// A 64-hex-character key file, such as a Docker secret. When given,
        /// credentials are kept in an encrypted file in the data folder
        /// instead of a credential store (ADR 0075).
        #[arg(long)]
        vault_key: Option<PathBuf>,
    },
    /// Print a new owner invite link (ADR 0076 §7).
    Invite {
        /// The folder holding the host's device list.
        #[arg(long)]
        data: PathBuf,
        /// The address desktops use to reach this host.
        #[arg(long)]
        public_address: String,
        /// The host's certificate, PEM, for the pin in the link.
        #[arg(long)]
        cert: PathBuf,
    },
    /// Exit 0 when something is listening at the address: a health check.
    Check {
        /// Where the host listens.
        #[arg(long, default_value = "127.0.0.1:7443")]
        listen: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: could not start: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli.command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Serve {
            data,
            listen,
            public_address,
            cert,
            key,
            vault_key,
        } => {
            serve(
                &data,
                &listen,
                public_address.as_deref(),
                &cert,
                &key,
                vault_key,
            )
            .await
        }
        Command::Invite {
            data,
            public_address,
            cert,
        } => {
            let devices = open_devices(&data)?;
            println!("{}", owner_invite(&devices, &public_address, &cert)?);
            Ok(())
        }
        Command::Check { listen } => tokio::net::TcpStream::connect(&listen)
            .await
            .map(drop)
            .map_err(|error| format!("nothing is listening at {listen}: {error}")),
    }
}

async fn serve(
    data: &Path,
    listen: &str,
    public_address: Option<&str>,
    cert: &Path,
    key: &Path,
    vault_key: Option<PathBuf>,
) -> Result<(), String> {
    std::fs::create_dir_all(data)
        .map_err(|error| format!("could not create {}: {error}", data.display()))?;
    let resolver: Arc<dyn SecretResolver> = match vault_key {
        Some(key_file) => Arc::new(FileSecretVault::new(data.join("vault"), key_file)),
        None => Arc::new(PlatformSecretVault::new()),
    };
    // Questions go to the desktops connected to this host.
    let approvals = Arc::new(RemoteApprover::new());
    let plane = tramontana_composition::open_service_plane(
        &data.join("host.sqlite"),
        Arc::clone(&approvals) as Arc<dyn Approver>,
        resolver,
        0,
    )
    .await?;
    let devices = Arc::new(open_devices(data)?);
    // A host nobody has paired with prints an owner invite (ADR 0076 §7).
    if !devices.has_devices() {
        let address = public_address.unwrap_or(listen);
        println!(
            "No device is paired yet. Open this link in Tramontana within 15 minutes:\n{}",
            owner_invite(&devices, address, cert)?
        );
    }
    let acceptor = tramontana_host::acceptor(&read(cert)?, &read(key)?)
        .map_err(|error| format!("the certificate or key is not usable: {error}"))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| format!("could not listen on {listen}: {error}"))?;
    println!("Listening on {listen}.");
    let host = Arc::new(Host::new(Arc::new(plane.service), devices, approvals));
    let served = host.serve(listener, acceptor).await;
    let mut models = plane.models;
    models.shutdown().await;
    plane.apps.shutdown().await;
    plane.browser.shutdown().await;
    served.map_err(|error| format!("stopped listening: {error}"))
}

fn open_devices(data: &Path) -> Result<Devices, String> {
    Devices::open(data.join("devices.json"))
        .map_err(|error| format!("could not read the device list: {error}"))
}

fn owner_invite(devices: &Devices, address: &str, cert: &Path) -> Result<String, String> {
    let fingerprint = tramontana_host::fingerprint(&read(cert)?)
        .map_err(|error| format!("the certificate is not usable: {error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let code = devices
        .invite("owner", now)
        .map_err(|error| format!("could not save the invite: {error}"))?;
    Ok(format!(
        "tramontana://pair?host={address}&fingerprint={fingerprint}&code={}",
        code.expose()
    ))
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))
}
