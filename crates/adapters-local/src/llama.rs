//! Supervising a user-supplied `llama-server`.
//!
//! `BUILD_ORDER.md` §14 item 8 asks for version check, health, port
//! allocation, readiness, cancellation, logs, and cleanup. Each of those is a
//! way this can go wrong in front of a user, and the shape of this module is
//! mostly about failing legibly rather than about starting a process.
//!
//! Four decisions worth stating.
//!
//! **The binary is the user's.** Nothing is downloaded, and the path is not
//! guessed from `PATH` — a model runtime is executable code, and picking one
//! up implicitly is how you end up running something the user did not choose.
//!
//! **A child that dies is noticed immediately.** Readiness polls health, but
//! it also watches for the process exiting, so a wrong model path fails in
//! milliseconds with the runtime's own message instead of timing out after
//! thirty seconds with nothing to show.
//!
//! **The environment is cleared, not inherited.** A model runtime has no
//! business seeing the parent's environment, which on a developer machine
//! holds API keys (`SECURITY_AND_PERMISSIONS.md` §7). Only the few variables
//! the operating system needs to load a binary at all are put back.
//!
//! **Cleanup is not best-effort on Unix.** The child is started in its own
//! process group and the group is signalled, so a runtime that spawned helpers
//! cannot leave them behind. On Windows only the child itself is terminated:
//! the equivalent is a job object, and building one for a runtime that spawns
//! no children would be untested code pretending to be a guarantee. Recorded
//! here rather than implied.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tramontana_core::contracts::{CorrelationId, ErrorCode, ErrorEnvelope, Health};

/// The oldest `llama.cpp` server build this adapter is written against.
///
/// `llama.cpp` numbers its releases with a monotonically increasing build
/// number. Refusing an older one is not gatekeeping: the `/health` endpoint
/// this module depends on for readiness reports differently in earlier builds,
/// and "it started but never became ready" is a much worse message than "this
/// build is too old".
pub const MINIMUM_BUILD: u64 = 3000;

/// How long to wait for a graceful stop before insisting.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How many log lines to keep.
///
/// Bounded because a model runtime is chatty and this buffer is shown in
/// diagnostics: an unbounded one is a memory leak that only manifests during
/// a long session, which is the worst time to find it.
const LOG_LINES: usize = 200;

/// Longest single log line retained.
const LOG_LINE_BYTES: usize = 2000;

/// Which kind of local runtime to supervise.
///
/// Both speak the OpenAI chat-completions format on loopback, take `--model`,
/// `--host` and `--port`, and report readiness on `/health` with 200, or 503
/// while loading; that is why one supervisor serves both (ADR 0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    /// `llama-server` from llama.cpp, serving one GGUF model file.
    LlamaServer,
    /// `mlx_lm.server` from MLX LM, serving one MLX model folder. Apple
    /// Silicon only; it reports no build to check.
    MlxLm,
}

impl RuntimeKind {
    /// What the runtime is called, in messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LlamaServer => "llama-server",
            Self::MlxLm => "mlx_lm.server",
        }
    }
}

/// Whether MLX can run on this machine: macOS on Apple Silicon.
#[must_use]
pub const fn mlx_supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

/// What to start, and where.
#[derive(Debug, Clone)]
pub struct LlamaServerSpec {
    /// Which runtime this is.
    pub kind: RuntimeKind,
    /// The executable the user chose. Never discovered.
    pub binary: PathBuf,
    /// The model the user chose: a GGUF file for `llama-server`, a model
    /// folder for `mlx_lm.server`.
    pub model: PathBuf,
    /// Port to bind, or `None` to have one allocated.
    pub port: Option<u16>,
    /// Extra arguments, passed through verbatim.
    ///
    /// Never a credential: `SECURITY_AND_PERMISSIONS.md` §7 forbids secrets in
    /// command-line arguments, and a local runtime needs none.
    pub extra_args: Vec<String>,
    /// How long to wait for the server to report itself healthy.
    pub readiness_timeout: Duration,
    /// Where to record the runtime while it runs, so a later process can stop
    /// it if this one is killed outright (ADR 0030). `None` records nothing.
    pub records: Option<PathBuf>,
}

impl LlamaServerSpec {
    /// A spec with the usual bounds.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>, model: impl Into<PathBuf>) -> Self {
        Self {
            kind: RuntimeKind::LlamaServer,
            binary: binary.into(),
            model: model.into(),
            port: None,
            extra_args: Vec::new(),
            // Loading a large model from cold storage genuinely takes this
            // long. It is a bound on *silence*, not on the whole start: a
            // child that exits fails immediately whatever this says.
            readiness_timeout: Duration::from_secs(120),
            records: None,
        }
    }

    /// An `mlx_lm.server` serving the MLX model folder `model`, with the
    /// same bounds.
    #[must_use]
    pub fn mlx(binary: impl Into<PathBuf>, model: impl Into<PathBuf>) -> Self {
        Self {
            kind: RuntimeKind::MlxLm,
            ..Self::new(binary, model)
        }
    }
}

/// What a binary reported about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedVersion {
    /// The build number, when one could be read.
    pub build: Option<u64>,
    /// The line the binary printed, bounded and kept for diagnostics.
    pub reported: String,
}

/// Why a runtime could not be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StartError {
    /// The binary is missing, not executable, or not a file.
    #[error("the runtime's executable is not usable: {0}")]
    Binary(String),
    /// The model is missing, or is not the kind of path the runtime needs.
    #[error("the model is not usable: {0}")]
    Model(String),
    /// This runtime cannot run on this machine.
    #[error("{0}")]
    Unsupported(String),
    /// The binary is older than this adapter supports.
    #[error("llama-server build {found} is older than the supported minimum {minimum}")]
    TooOld {
        /// What the binary reported.
        found: u64,
        /// What this adapter needs.
        minimum: u64,
    },
    /// No port could be reserved.
    #[error("could not reserve a local port: {0}")]
    Port(String),
    /// The process would not start at all.
    #[error("could not start the runtime: {0}")]
    Spawn(String),
    /// The process exited before it became ready.
    #[error("the runtime exited before it was ready ({status}); last output: {tail}")]
    ExitedEarly {
        /// How it exited.
        status: String,
        /// The tail of what it printed, which is what actually explains it.
        tail: String,
    },
    /// The process stayed alive but never reported itself healthy.
    #[error("the runtime did not become ready within {seconds}s; last output: {tail}")]
    NotReady {
        /// The bound that elapsed.
        seconds: u64,
        /// The tail of what it printed.
        tail: String,
    },
}

impl From<StartError> for ErrorEnvelope {
    fn from(error: StartError) -> Self {
        let code = match error {
            StartError::Binary(_) | StartError::Model(_) | StartError::TooOld { .. } => {
                ErrorCode::InvalidRequest
            }
            StartError::Unsupported(_) => ErrorCode::CapabilityUnsupported,
            StartError::Port(_) | StartError::Spawn(_) => ErrorCode::Internal,
            StartError::ExitedEarly { .. } | StartError::NotReady { .. } => {
                ErrorCode::ProviderUnavailable
            }
        };
        Self::new(code, error.to_string(), CorrelationId::new("runtime.llama"))
    }
}

/// Ask a binary what it is, before trusting it to be a model runtime.
///
/// # Errors
///
/// [`StartError::Binary`] when the path is not a runnable file or the process
/// could not be run at all.
pub async fn probe_version(binary: &Path) -> Result<ProbedVersion, StartError> {
    if !binary.is_file() {
        return Err(StartError::Binary(format!(
            "{} is not a file",
            binary.display()
        )));
    }

    let output = base_command(binary)
        .arg("--version")
        .output()
        .await
        .map_err(|failure| StartError::Binary(format!("could not run it: {failure}")))?;

    // `llama-server --version` prints to stderr on some builds and stdout on
    // others. Reading both is cheaper than caring which.
    let mut reported = String::from_utf8_lossy(&output.stdout).into_owned();
    reported.push_str(&String::from_utf8_lossy(&output.stderr));
    let reported = reported.trim().to_owned();
    let reported = truncate(&reported, LOG_LINE_BYTES);

    Ok(ProbedVersion {
        build: build_number(&reported),
        reported,
    })
}

/// The accelerators a `llama-server` reports it can use, from
/// `--list-devices`, for the person to see where their model runs.
///
/// Best effort, and bounded: a build without the flag, or one that does not
/// answer within a few seconds, reports none, and that never stops the
/// runtime from starting.
pub async fn probe_devices(binary: &Path) -> Vec<String> {
    let run = base_command(binary).arg("--list-devices").output();
    match tokio::time::timeout(Duration::from_secs(10), run).await {
        Ok(Ok(output)) => {
            let mut printed = String::from_utf8_lossy(&output.stdout).into_owned();
            printed.push_str(&String::from_utf8_lossy(&output.stderr));
            parse_devices(&printed)
        }
        _ => Vec::new(),
    }
}

/// The devices in `--list-devices` output that have memory of their own.
///
/// Lines look like `  MTL0: Apple M3 Max (28753 MiB, 28753 MiB free)`. A
/// library that runs on the processor lists no memory — `BLAS: Accelerate
/// (0 MiB, 0 MiB free)` — and is not an accelerator, so it is left out.
#[must_use]
pub fn parse_devices(printed: &str) -> Vec<String> {
    printed
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("Available devices"))
        .skip(1)
        .filter_map(|line| {
            let (name, rest) = line.trim().split_once(": ")?;
            let (description, memory) = rest.rsplit_once(" (")?;
            let total = memory.split_once(" MiB")?.0.trim().parse::<u64>().ok()?;
            (total > 0).then(|| {
                format!(
                    "{} ({name}, {} GB)",
                    truncate(description, 120),
                    (total + 512) / 1024
                )
            })
        })
        .collect()
}

/// The build number in a `llama.cpp` version banner, if there is one.
///
/// The banner has looked like `version: 3527 (a1b2c3d)` for a long time, but
/// not forever, and a build that does not match is reported as unknown rather
/// than as zero — "we could not tell" and "it is ancient" are different facts,
/// and only one of them should refuse to start.
fn build_number(reported: &str) -> Option<u64> {
    // Current banners name the build: "version: 0.4.1-dev (build 10964,
    // commit b29c606e2)". The version there is a release number, and reading
    // it as a build made every current runtime look older than the minimum.
    if let Some(after) = reported.split("(build ").nth(1) {
        return leading_number(after).map(|(number, _)| number);
    }
    // Earlier ones put the build first: "version: 3527 (a1b2c3d)". A dotted
    // number with no build is a release, which says nothing about the build.
    let after = reported.split("version:").nth(1)?;
    match leading_number(after.trim_start())? {
        (_, Some('.')) => None,
        (number, _) => Some(number),
    }
}

/// The number `text` starts with, and the character after it.
fn leading_number(text: &str) -> Option<(u64, Option<char>)> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    let number = digits.parse().ok()?;
    Some((number, text[digits.len()..].chars().next()))
}

/// A running `llama-server` this process owns.
pub struct SupervisedLlamaServer {
    child: Option<Child>,
    port: u16,
    logs: Arc<Mutex<VecDeque<String>>>,
    /// The tasks reading the child's pipes.
    ///
    /// Held so their completion can be waited for. Nothing orders a child's
    /// write against these tasks being polled, so reading the log buffer the
    /// instant the process exits can find it empty — which is exactly when
    /// its output matters most.
    readers: Vec<tokio::task::JoinHandle<()>>,
    client: reqwest::Client,
    version: ProbedVersion,
    /// This runtime's record, removed when it stops.
    record: Option<PathBuf>,
    /// The key a request must carry, when the runtime can require one.
    api_key: Option<String>,
    /// The accelerators the runtime reported, in the person's words.
    accelerators: Vec<String>,
    /// Which runtime this is.
    kind: RuntimeKind,
}

impl std::fmt::Debug for SupervisedLlamaServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupervisedLlamaServer")
            .field("port", &self.port)
            .field("build", &self.version.build)
            .finish_non_exhaustive()
    }
}

impl SupervisedLlamaServer {
    /// Start a runtime and wait until it can serve a request.
    ///
    /// # Errors
    ///
    /// A [`StartError`] naming which step failed. Every variant carries enough
    /// to act on: a version, an exit status and the runtime's own output, or
    /// the bound that elapsed.
    pub async fn start(spec: &LlamaServerSpec) -> Result<Self, StartError> {
        let version = match spec.kind {
            RuntimeKind::LlamaServer => {
                if !spec.model.is_file() {
                    return Err(StartError::Model(format!(
                        "{} is not a file",
                        spec.model.display()
                    )));
                }
                let version = probe_version(&spec.binary).await?;
                if let Some(build) = version.build
                    && build < MINIMUM_BUILD
                {
                    return Err(StartError::TooOld {
                        found: build,
                        minimum: MINIMUM_BUILD,
                    });
                }
                version
            }
            RuntimeKind::MlxLm => {
                // Refused before anything is started: MLX needs Apple
                // Silicon, and elsewhere the endpoint is simply unavailable,
                // not a reason the application cannot open.
                if !mlx_supported() {
                    return Err(StartError::Unsupported(
                        "MLX runs only on a Mac with Apple Silicon".to_owned(),
                    ));
                }
                if !spec.model.is_dir() {
                    return Err(StartError::Model(format!(
                        "{} is not a folder; an MLX model is a folder of weights, tokenizer and config",
                        spec.model.display()
                    )));
                }
                if !spec.binary.is_file() {
                    return Err(StartError::Binary(format!(
                        "{} is not a file",
                        spec.binary.display()
                    )));
                }
                // `mlx_lm.server` has no `--version`, so there is no build
                // to check; its answering `/health` is what is trusted.
                ProbedVersion {
                    build: None,
                    reported: RuntimeKind::MlxLm.name().to_owned(),
                }
            }
        };

        let port = match spec.port {
            Some(port) => port,
            None => reserve_port().await?,
        };

        // Out of the box both runtimes answer any request that reaches their
        // port and tell a browser any page may read the answer, so a web page
        // could run prompts on the person's model and read them — the answer
        // even names the model's full path. `llama-server` is given a key
        // only this process knows, through its environment because a key in
        // the arguments is visible to every process on the machine
        // (`SECURITY_AND_PERMISSIONS.md` §7). `mlx_lm.server` has no key, so
        // it is told that no web origin may read it: a page can still make it
        // work, but cannot read what it says.
        let api_key = match spec.kind {
            RuntimeKind::LlamaServer => Some(uuid::Uuid::new_v4().simple().to_string()),
            RuntimeKind::MlxLm => None,
        };
        let mut command = base_command(&spec.binary);
        if let Some(key) = &api_key {
            command.env("LLAMA_API_KEY", key);
        }
        if spec.kind == RuntimeKind::MlxLm {
            command.arg("--allowed-origins").arg("none");
            // `mlx_lm.server` loads whatever a request names, downloading it
            // from Hugging Face when it is not a local path — a request from
            // anywhere could make it reach the network, and Local Only must
            // never. Offline, the Hugging Face library refuses instead.
            // Checked against mlx-lm 0.31.3.
            command
                .env("HF_HUB_OFFLINE", "1")
                .env("HF_HUB_DISABLE_TELEMETRY", "1");
        }
        command
            .arg("--model")
            .arg(&spec.model)
            // Loopback only. A model runtime that binds every interface is
            // reachable from the network, which is not what "local model"
            // means to the person who started it.
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .args(&spec.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        own_process_group(&mut command);

        let mut child = command
            .spawn()
            .map_err(|failure| StartError::Spawn(failure.to_string()))?;

        let logs = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_LINES)));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(capture(stdout, Arc::clone(&logs)));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(capture(stderr, Arc::clone(&logs)));
        }

        // Recorded while it runs, so a process that inherits the profile after
        // this one is killed outright can stop it (ADR 0030).
        let record = spec.records.as_deref().and_then(|directory| {
            let record = crate::runtime_records::RuntimeRecord {
                pid: child.id()?,
                binary: spec.binary.clone(),
                port,
            };
            match crate::runtime_records::write(directory, &record) {
                Ok(path) => Some(path),
                Err(failure) => {
                    tracing::warn!(
                        "could not record the runtime, so a crash would leave it running: {failure}"
                    );
                    None
                }
            }
        });

        let mut server = Self {
            child: Some(child),
            port,
            logs,
            readers,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .build()
                .map_err(|failure| StartError::Spawn(failure.to_string()))?,
            version,
            record,
            api_key,
            kind: spec.kind,
            accelerators: match spec.kind {
                RuntimeKind::LlamaServer => probe_devices(&spec.binary).await,
                // MLX runs on Apple silicon's GPU by construction.
                RuntimeKind::MlxLm => vec!["Apple silicon GPU (MLX)".to_owned()],
            },
        };

        match server.await_ready(spec.readiness_timeout).await {
            Ok(()) => Ok(server),
            Err(error) => {
                // A failed start must not leave a process behind, including
                // when the failure is "it never became ready" and the process
                // is still very much alive.
                server.shutdown().await;
                Err(error)
            }
        }
    }

    /// The accelerators this runtime reported it can use. Empty means none
    /// were reported: it may be running on the processor alone.
    #[must_use]
    pub fn accelerators(&self) -> &[String] {
        &self.accelerators
    }

    /// What a request's `model` field must say, when the runtime needs
    /// something other than the model's identifier: `default_model` for
    /// `mlx_lm.server`, its name for the model it was started with.
    #[must_use]
    pub fn wire_model(&self) -> Option<&'static str> {
        match self.kind {
            RuntimeKind::LlamaServer => None,
            RuntimeKind::MlxLm => Some("default_model"),
        }
    }

    /// The key every model request must carry, when this runtime takes one.
    ///
    /// Never stored and never printed; it lives as long as this runtime.
    #[must_use]
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Where the runtime can be reached.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The port it was given.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// What the binary said it was.
    #[must_use]
    pub fn version(&self) -> &ProbedVersion {
        &self.version
    }

    /// The retained tail of the runtime's output.
    #[must_use]
    pub fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|logs| logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the runtime is serving right now.
    ///
    /// Distinguishes the three states that matter: gone, up but not serving,
    /// and serving. A health check that collapsed the first two would report
    /// a crashed runtime as merely busy.
    pub async fn health(&mut self) -> Health {
        if self.has_exited() {
            return Health::Unavailable;
        }
        match self.client.get(self.health_url()).send().await {
            Ok(response) if response.status().is_success() => Health::Available,
            // `llama-server` answers 503 while a model is still loading.
            Ok(_) => Health::Degraded,
            Err(_) => Health::Unavailable,
        }
    }

    /// Stop the runtime and wait for it to be gone.
    ///
    /// Idempotent: calling it twice, or after the process has already exited,
    /// is not an error. Cleanup that could fail on a second call is cleanup
    /// nobody dares put in an error path.
    pub async fn shutdown(&mut self) {
        let Some(mut child) = self.child.take() else {
            self.forget_record();
            return;
        };

        // Ask first. A model runtime that is mid-write to a cache should get
        // the chance to finish, and `llama-server` exits promptly on SIGTERM.
        request_stop(&child);
        let stopped = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
        if stopped.is_err() {
            // It did not go. This is the point of owning the process rather
            // than hoping.
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        self.forget_record();
    }

    /// Remove this runtime's record: it is stopping, so nothing is left
    /// behind to find.
    fn forget_record(&mut self) {
        if let Some(path) = self.record.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The health endpoint.
    fn health_url(&self) -> String {
        format!("http://127.0.0.1:{}/health", self.port)
    }

    /// Whether the child has already exited.
    fn has_exited(&mut self) -> bool {
        self.child
            .as_mut()
            .is_none_or(|child| matches!(child.try_wait(), Ok(Some(_)) | Err(_)))
    }

    /// Poll until the runtime serves, it dies, or the bound elapses.
    async fn await_ready(&mut self, bound: Duration) -> Result<(), StartError> {
        let deadline = tokio::time::Instant::now() + bound;

        loop {
            // Checked before health, so a runtime that refused its model is
            // reported in milliseconds with its own explanation rather than
            // after the full readiness bound with nothing useful.
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                let status = status.to_string();
                // Drain first. The process exiting says nothing about whether
                // the tasks reading its pipes have been polled, and an
                // `ExitedEarly` with an empty tail is the least useful
                // possible version of the most useful error this type has.
                self.drain_logs().await;
                return Err(StartError::ExitedEarly {
                    status,
                    tail: self.tail(),
                });
            }

            if matches!(self.health().await, Health::Available) {
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(StartError::NotReady {
                    seconds: bound.as_secs(),
                    tail: self.tail(),
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Wait for everything the child wrote to be in the buffer.
    ///
    /// Once the process is gone and these tasks hold the only read ends, EOF
    /// follows and they finish. The bound is not optional, though: a
    /// grandchild that inherited the pipes keeps them open after its parent
    /// exits, so without it this would wait for a process nobody is waiting
    /// on. A partial tail beats a hang.
    async fn drain_logs(&mut self) {
        let readers = std::mem::take(&mut self.readers);
        let drained = tokio::time::timeout(Duration::from_millis(500), async {
            for reader in readers {
                let _ = reader.await;
            }
        })
        .await;
        if drained.is_err() {
            tracing::debug!("the runtime's output was still open after it exited");
        }
    }

    /// The last few log lines, for an error message.
    fn tail(&self) -> String {
        let logs = self.logs();
        let start = logs.len().saturating_sub(5);
        logs[start..].join(" | ")
    }
}

impl Drop for SupervisedLlamaServer {
    /// Kill an abandoned runtime rather than orphaning it.
    ///
    /// `shutdown` is the graceful path and should be awaited. This exists for
    /// the paths nobody plans: a panic, an early return, a test that fails
    /// halfway. Leaving a model runtime holding several gigabytes of memory
    /// because a future was dropped is not an acceptable failure mode.
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            request_stop(child);
            let _ = child.start_kill();
        }
        self.forget_record();
    }
}

/// A command with the parent's environment removed.
///
/// Cleared rather than inherited: on a developer machine the environment holds
/// API keys, and a model runtime has no business seeing them
/// (`SECURITY_AND_PERMISSIONS.md` §7). Only what an operating system needs to
/// load and run a binary at all is put back.
fn base_command(binary: &Path) -> Command {
    let mut command = Command::new(binary);
    command.env_clear();

    // `PATH` so a runtime can find a sibling helper it ships with. Nothing
    // else on Unix.
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    // Windows cannot load a DLL without these, so clearing them outright makes
    // the process fail to start rather than run without secrets.
    #[cfg(windows)]
    for name in [
        "SystemRoot",
        "SystemDrive",
        "TEMP",
        "TMP",
        "NUMBER_OF_PROCESSORS",
    ] {
        if let Ok(value) = std::env::var(name) {
            command.env(name, value);
        }
    }
    command
}

/// Put the child in its own process group, where the platform has them.
#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    // Group id 0 means "a new group led by the child", so signalling the group
    // reaches the runtime and anything it spawned, and never reaches us.
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {
    // A job object is the Windows equivalent. Not built here: `llama-server`
    // spawns no children, and untested cleanup code that looks like a
    // guarantee is worse than a documented limit.
}

/// Ask a child to stop, without insisting.
#[cfg(unix)]
fn request_stop(child: &Child) {
    let Some(id) = child.id() else { return };
    let Ok(pid) = i32::try_from(id)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or(())
    else {
        return;
    };
    // The whole group, which is why the child was given one. A failure here
    // means the group is already gone, which is the outcome being asked for.
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
}

#[cfg(not(unix))]
fn request_stop(_child: &Child) {
    // No graceful signal to send; `kill` below terminates the child.
}

/// First port of the range runtimes are given.
///
/// Below every operating system's default ephemeral range — 32768 on Linux,
/// 49152 on macOS and Windows — so the kernel never hands one of these out on
/// its own. That is the whole point of choosing a range at all.
const RESERVED_PORT_BASE: u16 = 20_000;

/// How many ports the range spans. Ends at 31999, below Linux's 32768.
const RESERVED_PORT_SPAN: u16 = 12_000;

/// Candidates tried before giving up.
///
/// Each one that fails is a port something is already listening on, so
/// running out means the range is genuinely crowded, not that the search was
/// unlucky.
const RESERVE_ATTEMPTS: u16 = 64;

/// The next candidate offset, shared by every reservation in this process.
///
/// Concurrent reservations take different values, so two runtimes started at
/// the same moment by one application never probe — and never receive — the
/// same port.
static NEXT_CANDIDATE: AtomicU16 = AtomicU16::new(0);

/// Reserve a free loopback port.
///
/// Choose a candidate, check nothing is listening on it, and hand the number
/// to the runtime. Handing a number to another process always leaves a window
/// before the runtime binds it; the design is about who can take the port
/// inside that window, and the answer should be "nobody we control".
///
/// Three pieces, each load-bearing:
///
/// - **A range outside every default ephemeral range**, so no outbound
///   connection can be holding the port: the probe below only sees
///   listeners, and here only a listener could be in the way.
/// - **A process-wide counter**, so two runtimes started at the same moment by
///   one application never receive the same port. A probe cannot see another
///   reservation that has not bound yet; distinct candidates make that moot.
/// - **A probe that leaves nothing listening** (see [`is_free`]).
///
/// This took two wrong versions to find. The first bound port 0, released it,
/// and failed about one parallel test run in eight with the runtime unable to
/// bind. I moved to this range on the theory that the kernel was re-issuing
/// the released ephemeral port — and it still failed three runs in sixty, on
/// ports the kernel never issues. Capturing the holder at the moment of
/// failure showed the real cause: a *sibling* runtime process listening on
/// the port at fd 32, the same descriptor number the probe had in the parent.
/// It had inherited the probe socket. See [`is_free`].
async fn reserve_port() -> Result<u16, StartError> {
    // Start where it differs between processes, so two applications reserving
    // at once do not walk the same sequence.
    let seed = std::process::id() % u32::from(RESERVED_PORT_SPAN);

    for _ in 0..RESERVE_ATTEMPTS {
        let step = NEXT_CANDIDATE.fetch_add(1, Ordering::Relaxed);
        let offset = (seed + u32::from(step)) % u32::from(RESERVED_PORT_SPAN);
        let port = RESERVED_PORT_BASE + u16::try_from(offset).unwrap_or(0);
        // Something listens there: move on. A port in use is the ordinary case
        // this loop exists for.
        if is_free(port).await {
            return Ok(port);
        }
    }

    Err(StartError::Port(format!(
        "no free port among {RESERVE_ATTEMPTS} candidates in {RESERVED_PORT_BASE}..{}",
        RESERVED_PORT_BASE + RESERVED_PORT_SPAN
    )))
}

/// Whether nothing is listening on `port`.
///
/// Probed by *connecting*, not by binding. A bind probe is a listening socket
/// in this process, and on macOS a child spawned by another thread at that
/// moment inherits it: macOS has no atomic `SOCK_CLOEXEC`, so a new socket is
/// inheritable until a separate call marks it close-on-exec. The inheriting
/// child then holds the port open after this process has released it, and the
/// runtime the port was reserved for cannot bind. A connection attempt leaves
/// nothing listening behind, whoever spawns what, and whenever.
///
/// Refusal is immediate on a Unix loopback interface, and in the reserved
/// range nothing but a listener can be holding the port, so "refused" means
/// "free". The timeout covers a loopback that drops rather than refuses.
#[cfg(unix)]
async fn is_free(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused
    )
}

/// Whether nothing is listening on `port`.
///
/// Probed by binding here, the opposite choice to Unix, for two Windows
/// reasons: sockets are created non-inheritable, so the macOS failure cannot
/// occur; and a connection to a closed loopback port is not refused promptly
/// — the stack retries before reporting it — which would make a connect probe
/// slow enough to exhaust the candidates.
#[cfg(not(unix))]
async fn is_free(port: u16) -> bool {
    tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .is_ok()
}

/// Read a child's output into the bounded buffer.
fn capture<R>(stream: R, logs: Arc<Mutex<VecDeque<String>>>) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // Redacted on the way in. A runtime that echoes a configuration
            // file could print a credential, and this buffer is shown in
            // diagnostics and carried into an export.
            let line = tramontana_core::services::redact(&line).text;
            let Ok(mut logs) = logs.lock() else { return };
            if logs.len() == LOG_LINES {
                logs.pop_front();
            }
            logs.push_back(truncate(&line, LOG_LINE_BYTES));
        }
    })
}

/// Cut a string to a byte bound without splitting a character.
fn truncate(value: &str, bound: usize) -> String {
    if value.len() <= bound {
        return value.to_owned();
    }
    let mut end = bound;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reserved_ports_come_from_outside_every_default_ephemeral_range() {
        // The property the fix rests on. Port 0 draws from the pool the kernel
        // re-issues to outbound connections; this range is never drawn from.
        for _ in 0..20 {
            let port = reserve_port().await.unwrap();
            assert!(
                (RESERVED_PORT_BASE..RESERVED_PORT_BASE + RESERVED_PORT_SPAN).contains(&port),
                "{port} is outside the reserved range"
            );
            // Linux starts its ephemeral range at 32768; macOS and Windows at 49152.
            assert!(port < 32_768, "{port} is inside an ephemeral range");
        }
    }

    #[tokio::test]
    async fn a_port_something_listens_on_is_not_free_and_is_once_it_stops() {
        let port = reserve_port().await.unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        assert!(
            !is_free(port).await,
            "{port} has a listener but was reported free"
        );
        drop(listener);
        assert!(
            is_free(port).await,
            "{port} was reported busy after its listener closed"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_reservations_in_one_process_never_collide() {
        let handles: Vec<_> = (0..64).map(|_| tokio::spawn(reserve_port())).collect();
        let mut ports = Vec::new();
        for handle in handles {
            ports.push(handle.await.unwrap().unwrap());
        }
        let unique: std::collections::HashSet<_> = ports.iter().collect();
        assert_eq!(
            unique.len(),
            ports.len(),
            "two concurrent reservations got one port"
        );
    }

    #[test]
    fn a_version_banner_yields_its_build_number() {
        assert_eq!(
            build_number("version: 3527 (a1b2c3d)\nbuilt with clang"),
            Some(3527)
        );
        assert_eq!(build_number("llama-server version: 9999"), Some(9999));
        // The banner of b10964, the release V3 runs on.
        assert_eq!(
            build_number(
                "version: 0.4.1-dev (build 10964, commit b29c606e2)\nbuilt with AppleClang 21.0.0.21000101 for Darwin arm64"
            ),
            Some(10964)
        );
    }

    #[test]
    fn an_unreadable_banner_is_unknown_rather_than_zero() {
        // "we could not tell" and "it is ancient" are different facts, and
        // only one of them should refuse to start.
        assert_eq!(build_number("some future format"), None);
        assert_eq!(build_number(""), None);
        assert_eq!(build_number("version: unknown"), None);
        // A release number is not a build number.
        assert_eq!(build_number("version: 0.5.0 (commit abcdef0)"), None);
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let multibyte = "é".repeat(50);
        let cut = truncate(&multibyte, 11);
        assert!(cut.ends_with('…'));
        // The point: this would panic on a byte slice through a character.
        assert!(cut.chars().count() > 1);
    }

    #[test]
    fn devices_with_memory_are_accelerators_and_a_processor_library_is_not() {
        // What llama-server build 10964 prints on an M3 Max.
        let printed = "load_backend: loaded stuff\nAvailable devices:\n  MTL0: Apple M3 Max (28753 MiB, 28753 MiB free)\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n";
        assert_eq!(parse_devices(printed), ["Apple M3 Max (MTL0, 28 GB)"]);
        // A GPU on another platform, and a machine with none.
        assert_eq!(
            parse_devices(
                "Available devices:\n  CUDA0: NVIDIA GeForce RTX 4090 (24080 MiB, 23000 MiB free)\n"
            ),
            ["NVIDIA GeForce RTX 4090 (CUDA0, 24 GB)"]
        );
        assert!(parse_devices("Available devices:\n").is_empty());
        assert!(parse_devices("error: unknown argument: --list-devices").is_empty());
    }
}
