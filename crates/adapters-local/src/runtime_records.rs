//! Records of the runtimes a profile started, so one left behind can be
//! stopped later.
//!
//! A supervised runtime is stopped when its owner shuts it down or drops it. A
//! process killed outright, by `kill -9` or a crash, does neither, and its
//! runtime lives on holding gigabytes of memory. So each runtime writes a small
//! record beside the profile when it starts, and removes it when it stops. The
//! next process to hold the profile's lease knows the previous holder is gone,
//! and stops any recorded runtime still running.
//!
//! A process id can be reused. A record is acted on only if the process with
//! that id still runs the recorded binary on the recorded port. Anything else
//! is left alone, and only the record is removed.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// One runtime, as its record describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRecord {
    /// The runtime's process id, which also names its process group.
    pub pid: u32,
    /// The executable it was started from.
    pub binary: PathBuf,
    /// The loopback port it was given.
    pub port: u16,
}

impl RuntimeRecord {
    fn to_json(&self) -> String {
        serde_json::json!({
            "pid": self.pid,
            "binary": self.binary.to_string_lossy(),
            "port": self.port,
        })
        .to_string()
    }

    fn from_json(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Self {
            pid: u32::try_from(value["pid"].as_u64()?).ok()?,
            binary: PathBuf::from(value["binary"].as_str()?),
            port: u16::try_from(value["port"].as_u64()?).ok()?,
        })
    }
}

/// Write the record for a runtime that has just started.
///
/// Returns the record's path, to remove when the runtime stops.
///
/// # Errors
///
/// When the directory cannot be created or the record written.
pub fn write(directory: &Path, record: &RuntimeRecord) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("{}.json", record.pid));
    std::fs::write(&path, record.to_json())?;
    Ok(path)
}

/// What became of one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaped {
    /// The runtime was still running, and was stopped.
    Stopped(RuntimeRecord),
    /// The runtime had already gone.
    AlreadyGone(RuntimeRecord),
    /// Another program now has that process id, and was left alone.
    NotOurs(RuntimeRecord),
    /// The record could not be read.
    Unreadable(PathBuf),
}

/// How long a runtime is given to stop after being asked, before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Stop every recorded runtime still running, and remove the records.
///
/// Call only while holding the profile's lease: that is what proves the
/// process that started them has gone, rather than being another process
/// using them right now.
pub async fn reap(directory: &Path) -> Vec<Reaped> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut reaped = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let record = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| RuntimeRecord::from_json(&text));
        let outcome = match record {
            None => Reaped::Unreadable(path.clone()),
            Some(record) => stop_if_ours(record).await,
        };
        let _ = std::fs::remove_file(&path);
        reaped.push(outcome);
    }
    reaped
}

/// Stop the recorded runtime if its process id still runs what was recorded.
async fn stop_if_ours(record: RuntimeRecord) -> Reaped {
    match command_line(record.pid).await {
        None => Reaped::AlreadyGone(record),
        Some(command) if !is_ours(&command, &record) => Reaped::NotOurs(record),
        Some(_) => {
            signal_group(record.pid, Stop::Ask);
            let asked = tokio::time::Instant::now();
            while asked.elapsed() < STOP_GRACE {
                if !still_ours(&record).await {
                    return Reaped::Stopped(record);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            signal_group(record.pid, Stop::Kill);
            Reaped::Stopped(record)
        }
    }
}

async fn still_ours(record: &RuntimeRecord) -> bool {
    command_line(record.pid)
        .await
        .is_some_and(|command| is_ours(&command, record))
}

/// Whether a command line is the recorded runtime: its binary, on its port.
fn is_ours(command: &str, record: &RuntimeRecord) -> bool {
    command.contains(&*record.binary.to_string_lossy())
        && command.contains(&format!("--port {}", record.port))
}

/// The command line of a running process, or `None` when there is none.
///
/// Asked of `ps`, which reads it on both macOS and Linux without unsafe code.
/// A process that has exited but not been waited for reports no command line
/// of its own, so it counts as gone.
async fn command_line(pid: u32) -> Option<String> {
    let output = tokio::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;
    let command = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !command.is_empty()).then_some(command)
}

enum Stop {
    Ask,
    Kill,
}

#[cfg(unix)]
fn signal_group(pid: u32, stop: Stop) {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    let signal = match stop {
        Stop::Ask => rustix::process::Signal::TERM,
        Stop::Kill => rustix::process::Signal::KILL,
    };
    // The runtime leads its own group, so this reaches it and anything it
    // started. A failure means the group is already gone.
    let _ = rustix::process::kill_process_group(pid, signal);
}

#[cfg(not(unix))]
fn signal_group(_pid: u32, _stop: Stop) {
    // Runtimes are supervised on Unix only; see the crate documentation.
}
