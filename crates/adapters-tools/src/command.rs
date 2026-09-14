//! The command broker: every process this crate starts goes through here.
//!
//! `BUILD_ORDER.md` §16 item 5 and `SECURITY_AND_PERMISSIONS.md` §6. A command
//! is a program and an argument array — never a shell string — run in a
//! working directory the caller has already resolved inside the grant, with a
//! cleared environment plus an explicit allowlist, no standard input, a
//! deadline, and bounded output.
//!
//! **The broker owns the whole process tree.** The command starts as the
//! leader of a new process group (a job object on Windows), and the group is
//! killed however the run ends: when the deadline passes, when it is
//! cancelled, when the caller stops waiting, and when the leader exits
//! normally but left children behind — which is how a test runner's forgotten
//! helper would otherwise become an orphan.
//!
//! This is **mediation, not containment** (`BUILD_ORDER.md` §16). A process
//! that starts its own session or group leaves the tree the broker owns, and
//! nothing here limits what a command can read, write, or reach over the
//! network. ADR 0024 records what is and is not enforced.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio_util::sync::CancellationToken;

/// How long output may keep arriving once the tree has been killed.
///
/// Killing the group closes every pipe its members held. A stream still open
/// after this long is held by something that escaped the group, and waiting
/// for it would hand the deadline to a process the broker no longer owns.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Variables a general command may inherit, when they are set: enough to find
/// programs, a home, a locale and a temporary directory. None is meant to
/// carry a credential; anything else must be passed deliberately.
pub const BASE_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TMP",
    "TEMP",
    // Windows needs these to start most programs at all.
    "SystemRoot",
    "windir",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
];

/// One command, fully described before anything runs.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// The program: a name looked up on the inherited `PATH`, or a path.
    pub program: OsString,
    /// Its arguments, each passed as exactly one argument. No shell ever
    /// joins, splits or expands them.
    pub arguments: Vec<OsString>,
    /// Where it runs. The caller resolves this inside the grant; the broker
    /// checks only that it is a directory.
    pub directory: PathBuf,
    /// Variables copied from this process, when set. Everything else is
    /// cleared.
    pub inherit: Vec<String>,
    /// Variables set deliberately, after the inherited ones.
    pub set: Vec<(String, OsString)>,
    /// How long the whole run may take.
    pub deadline: Duration,
    /// How many bytes of each of standard output and standard error to keep.
    pub output_limit: usize,
}

/// What a command that ran to its end produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// The exit code, or `None` when a signal ended the command.
    pub exit_code: Option<i32>,
    /// Standard output, up to the limit.
    pub stdout: Stream,
    /// Standard error, up to the limit.
    pub stderr: Stream,
}

impl CommandOutput {
    /// Whether the command exited with status 0.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// One captured output stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stream {
    /// The bytes kept: at most the spec's limit.
    pub kept: Vec<u8>,
    /// How many bytes arrived in all.
    pub total: u64,
    /// Whether reading stopped before the stream ended, because something
    /// outside the killed tree still held it open.
    pub cut_off: bool,
}

impl Stream {
    /// Whether anything was written that is not in [`Self::kept`].
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.cut_off || self.total > self.kept.len() as u64
    }
}

/// Why a command produced no output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    /// The working directory is not a directory.
    NoDirectory,
    /// An environment variable name is empty or malformed.
    InvalidEnvironment,
    /// The program could not be started.
    Spawn(std::io::ErrorKind),
    /// Waiting on the program failed; the tree was killed.
    Wait(std::io::ErrorKind),
    /// The deadline passed; the tree was killed.
    Timeout,
    /// The run was cancelled; the tree was killed.
    Cancelled,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDirectory => write!(f, "the working directory does not exist"),
            Self::InvalidEnvironment => write!(f, "an environment variable name is invalid"),
            Self::Spawn(kind) => write!(f, "the program could not be started: {kind}"),
            Self::Wait(kind) => write!(f, "the program could not be waited for: {kind}"),
            Self::Timeout => write!(f, "the command exceeded its deadline"),
            Self::Cancelled => write!(f, "the command was cancelled"),
        }
    }
}

impl std::error::Error for CommandError {}

/// Run one command to its end, owning every process it starts.
///
/// # Errors
///
/// [`CommandError`] when nothing could run, or when the run was cut short.
/// Whatever the error, no process the command started is left running —
/// within the limit of what a process group can hold (ADR 0024).
pub async fn run_command(
    spec: &CommandSpec,
    cancel: &CancellationToken,
) -> Result<CommandOutput, CommandError> {
    if cancel.is_cancelled() {
        return Err(CommandError::Cancelled);
    }
    if !spec.directory.is_dir() {
        return Err(CommandError::NoDirectory);
    }
    if !spec
        .inherit
        .iter()
        .chain(spec.set.iter().map(|(name, _)| name))
        .all(|name| valid_name(name))
    {
        return Err(CommandError::InvalidEnvironment);
    }

    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.arguments)
        .current_dir(&spec.directory)
        .env_clear();
    for name in &spec.inherit {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in &spec.set {
        command.env(name, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    {
        wrapped.wrap(process_wrap::tokio::JobObject);
        wrapped.wrap(process_wrap::tokio::KillOnDrop);
    }
    let child = wrapped
        .spawn()
        .map_err(|error| CommandError::Spawn(error.kind()))?;
    let mut tree = OwnedTree {
        child,
        settled: false,
    };

    let stdout = Capture::start(tree.child.stdout().take(), spec.output_limit);
    let stderr = Capture::start(tree.child.stderr().take(), spec.output_limit);

    let ended = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(CommandError::Cancelled),
        () = tokio::time::sleep(spec.deadline) => Err(CommandError::Timeout),
        status = tree.child.wait() => status.map_err(|error| CommandError::Wait(error.kind())),
    };
    // However it ended — even by exiting normally — nothing it started
    // outlives it.
    tree.kill().await;

    match ended {
        Ok(status) => Ok(CommandOutput {
            exit_code: status.code(),
            stdout: stdout.finish().await,
            stderr: stderr.finish().await,
        }),
        Err(error) => {
            stdout.abandon();
            stderr.abandon();
            Err(error)
        }
    }
}

/// Whether `name` can be an environment variable name on every platform.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}

/// The spawned tree, killed when dropped unless it has been settled.
///
/// A caller that stops waiting drops the broker's future, and that must not
/// leave the tree running. `start_kill` signals the whole group without
/// blocking, so it is safe in `drop`.
struct OwnedTree {
    child: Box<dyn ChildWrapper>,
    settled: bool,
}

impl OwnedTree {
    /// Kill every process in the tree, and reap the leader within the grace.
    async fn kill(&mut self) {
        // After a normal exit the group may already be empty: then there is
        // nothing to signal, and the error says only that.
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(DRAIN_GRACE, self.child.wait()).await;
        self.settled = true;
    }
}

impl Drop for OwnedTree {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.child.start_kill();
        }
    }
}

/// A stream being read in the background, into a buffer the broker can take
/// back even if the reader never finishes.
struct Capture {
    stream: Arc<Mutex<Stream>>,
    task: tokio::task::JoinHandle<()>,
}

impl Capture {
    /// Read `reader` to its end, keeping at most `limit` bytes.
    ///
    /// Reading continues past the limit, discarding, so a command that
    /// writes a great deal never blocks on a full pipe.
    fn start<R>(reader: Option<R>, limit: usize) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let stream = Arc::new(Mutex::new(Stream::default()));
        let shared = Arc::clone(&stream);
        let task = tokio::spawn(async move {
            let Some(mut reader) = reader else {
                return;
            };
            let mut chunk = vec![0; 8192];
            loop {
                let read = match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => read,
                };
                let Ok(mut stream) = shared.lock() else {
                    return;
                };
                stream.total += read as u64;
                let room = limit.saturating_sub(stream.kept.len());
                stream.kept.extend_from_slice(&chunk[..read.min(room)]);
            }
        });
        Self { stream, task }
    }

    /// What was read, once the stream ends or the grace runs out.
    async fn finish(self) -> Stream {
        let abort = self.task.abort_handle();
        let ended = tokio::time::timeout(DRAIN_GRACE, self.task).await;
        abort.abort();
        let mut stream = self
            .stream
            .lock()
            .map(|stream| stream.clone())
            .unwrap_or_default();
        stream.cut_off = !matches!(ended, Ok(Ok(())));
        stream
    }

    /// Stop reading; the output is not wanted.
    fn abandon(self) {
        self.task.abort();
    }
}
