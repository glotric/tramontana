use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, ProviderId, SideEffect,
    ToolDescriptor,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};

use std::ffi::OsString;
use std::path::Path;

use crate::command::{CommandError, CommandSpec, run_command};
use crate::limits::ReadLimits;
use crate::scope::primary_root;

/// Read-only Git inspection.
///
/// `git` runs through the command broker: a fixed argument array and **no
/// shell**, its working directory pinned to an approved root, a minimal
/// environment, a deadline, a bounded output, and a process tree the broker
/// owns. The arguments are hard-coded rather than composed from caller input.
///
/// Read-only in the sense that matters: the subcommands cannot alter the
/// repository. `SECURITY_AND_PERMISSIONS.md` §3.1 classifies Git status and
/// diff as Read.
///
/// **A repository cannot make these reads run a program**
/// (`SECURITY_AND_PERMISSIONS.md` §5). Git runs programs during plain reads
/// whenever its configuration names one: a filesystem monitor, a filter
/// driver or a text converter while `status` and `diff` compare files, a
/// signature program during `log`, a hook whenever `status` rewrites the
/// index. A repository can supply that configuration, in its own
/// `.git/config` or, while `HOME` pointed at the project, in a committed
/// `.gitconfig`. So:
///
/// - no global configuration is read at all;
/// - every setting that names such a program is overridden on the command
///   line, which outranks every configuration file;
/// - each filter driver the configuration defines is switched off by name
///   before the read, and a name that cannot be switched off stops the read;
/// - submodules, whose configuration these reads never see, are not entered.
///
/// Each is tested by arming the program and checking that it never runs. This
/// is mediation, not containment: a setting a later Git adds that runs a
/// program would need adding here too.
#[derive(Debug, Clone)]
pub struct GitTools {
    limits: ReadLimits,
}

impl Default for GitTools {
    fn default() -> Self {
        Self::new()
    }
}

impl GitTools {
    /// Tools with the default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: ReadLimits::default(),
        }
    }

    /// Tools with explicit limits.
    #[must_use]
    pub fn with_limits(limits: ReadLimits) -> Self {
        Self { limits }
    }

    /// How every Git process these tools start is run.
    fn spec(&self, directory: &Path, arguments: Vec<OsString>, output_limit: usize) -> CommandSpec {
        CommandSpec {
            program: "git".into(),
            arguments,
            directory: directory.to_path_buf(),
            // A scrubbed environment. Inheriting the parent's would hand the
            // child every variable this process holds, and Git in particular
            // reads a great deal of behavior from its environment. `HOME` is
            // deliberately absent: pointing it at the project once made a
            // committed `.gitconfig` Git's global configuration.
            inherit: vec!["PATH".to_owned()],
            set: vec![
                ("GIT_TERMINAL_PROMPT".to_owned(), "0".into()),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".into()),
                ("GIT_CONFIG_GLOBAL".to_owned(), NO_CONFIGURATION.into()),
            ],
            deadline: self.limits.deadline,
            output_limit,
        }
    }
}

pub(crate) const STATUS: &str = "builtin.git.status";
pub(crate) const DIFF: &str = "builtin.git.diff";
pub(crate) const LOG: &str = "builtin.git.log";

/// Settings that hold whatever the repository's configuration says: `-c`
/// outranks every configuration file. Each would otherwise let the
/// repository name a program these reads run.
const HARDENING: &[&str] = &[
    // A filesystem monitor is a program `status` and `diff` run.
    "-c",
    "core.fsmonitor=false",
    // Showing signatures runs `gpg.program`.
    "-c",
    "log.showSignature=false",
    // Hooks are programs too, and `status` fires one when it rewrites the
    // index.
    "-c",
    HOOKS_OFF,
    // A read has no business rewriting the index at all.
    "--no-optional-locks",
];

/// Where Git reads its "global" configuration from: nowhere.
///
/// With this empty and `HOME` unset, neither the user's own `~/.gitconfig` nor
/// a `.gitconfig` committed to the project can configure these reads.
#[cfg(windows)]
const NO_CONFIGURATION: &str = "NUL";
#[cfg(not(windows))]
const NO_CONFIGURATION: &str = "/dev/null";

/// Hooks are looked for in a directory that cannot hold any.
#[cfg(windows)]
const HOOKS_OFF: &str = "core.hooksPath=NUL";
#[cfg(not(windows))]
const HOOKS_OFF: &str = "core.hooksPath=/dev/null";

/// Lists every filter setting the configuration defines. Reading
/// configuration runs nothing.
const CONFIGURED_FILTERS: &[&str] = &[
    "config",
    "--null",
    "--name-only",
    "--get-regexp",
    "^filter\\.",
];

/// The largest listing of filter settings read before refusing.
const MAX_LISTING_BYTES: usize = 64 * 1024;

/// The most filter drivers a repository may define before these reads refuse.
const MAX_FILTER_DRIVERS: usize = 64;

/// The filter drivers named in `git config --null --name-only` output.
///
/// `None` means refuse to read: a driver's name could not be handed back to
/// Git intact on the command line (it holds `=`, which would end a `-c` key,
/// or a control character), or there are more than [`MAX_FILTER_DRIVERS`].
fn filter_drivers(listing: &[u8]) -> Option<Vec<String>> {
    let mut drivers: Vec<String> = Vec::new();
    for key in listing
        .split(|&byte| byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = std::str::from_utf8(key).ok()?;
        let (name, _variable) = key.strip_prefix("filter.")?.rsplit_once('.')?;
        if name.is_empty() || name.contains('=') || name.chars().any(char::is_control) {
            return None;
        }
        if !drivers.iter().any(|driver| driver == name) {
            drivers.push(name.to_owned());
        }
        if drivers.len() > MAX_FILTER_DRIVERS {
            return None;
        }
    }
    Some(drivers)
}

/// The exact arguments each tool runs. Nothing here comes from the caller.
fn arguments_for(tool_id: &str) -> Option<&'static [&'static str]> {
    match tool_id {
        // Submodules have configuration of their own, which these reads never
        // see, so they are not entered.
        STATUS => Some(&[
            "status",
            "--porcelain=v1",
            "--branch",
            "--ignore-submodules=all",
        ]),
        // A per-file summary, then the patch itself: which files changed and
        // by how much, then what changed in them. `--no-color` keeps the
        // output plain; `--no-ext-diff` and `--no-textconv` stop the
        // repository's configuration from running an external diff program
        // or a text converter.
        DIFF => Some(&[
            "--no-pager",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=all",
            "--stat",
            "--patch",
        ]),
        LOG => Some(&[
            "--no-pager",
            "log",
            "--max-count=20",
            "--no-color",
            "--pretty=format:%h %ad %s",
            "--date=short",
        ]),
        _ => None,
    }
}

fn descriptor(tool_id: &str, display: &str, description: &str) -> ToolDescriptor {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    });
    ToolDescriptor::new(tool_id, "builtin", display, description, schema, "git.read")
}

#[async_trait]
impl ToolProvider for GitTools {
    fn id(&self) -> ProviderId {
        ProviderId::new("builtin")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![
            descriptor(STATUS, "Git status", "Show the working tree status."),
            descriptor(
                DIFF,
                "Git diff",
                "Show uncommitted changes to tracked files: which files changed and by how much, \
                 then the patch itself. A new file Git does not track yet is listed by \
                 builtin.git.status instead.",
            ),
            descriptor(LOG, "Git log", "Show recent commits."),
        ]
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let correlation = CorrelationId::new(request.invocation_id().as_str());
        let fail = |code, message: String| ErrorEnvelope::new(code, message, correlation.clone());

        if cancel.is_cancelled() {
            return Err(fail(
                ErrorCode::Cancelled,
                "cancelled before running".to_owned(),
            ));
        }

        let Some(arguments) = arguments_for(request.tool_id()) else {
            return Err(fail(
                ErrorCode::InvalidRequest,
                format!("unknown tool {}", request.tool_id()),
            ));
        };
        let root = primary_root(&request)?;
        let broker_error = |error: CommandError| match error {
            CommandError::Cancelled => {
                fail(ErrorCode::Cancelled, "cancelled while running".to_owned())
            }
            CommandError::Timeout => {
                fail(ErrorCode::Timeout, "git exceeded its deadline".to_owned())
            }
            other => fail(ErrorCode::ToolFailed, format!("could not run git: {other}")),
        };

        // Every filter driver the configuration defines is switched off
        // before the read. Listing them runs nothing, and a driver that
        // cannot be switched off means not reading at all.
        let listing = run_command(
            &self.spec(
                root.as_path(),
                HARDENING
                    .iter()
                    .chain(CONFIGURED_FILTERS)
                    .map(OsString::from)
                    .collect(),
                MAX_LISTING_BYTES,
            ),
            &cancel,
        )
        .await
        .map_err(&broker_error)?;
        let drivers = match listing.exit_code {
            // `--get-regexp` exits 1 when nothing matches.
            Some(1) => Some(Vec::new()),
            Some(0) if !listing.stdout.truncated() => filter_drivers(&listing.stdout.kept),
            _ => None,
        }
        .ok_or_else(|| {
            fail(
                ErrorCode::ToolFailed,
                "the repository's Git configuration defines filters this read cannot switch \
                 off; nothing was read"
                    .to_owned(),
            )
        })?;
        let switched_off = drivers.iter().flat_map(|name| {
            ["clean", "smudge", "process"]
                .into_iter()
                .map(move |variable| format!("filter.{name}.{variable}="))
                .chain(std::iter::once(format!("filter.{name}.required=false")))
                .flat_map(|setting| [OsString::from("-c"), OsString::from(setting)])
        });

        let spec = self.spec(
            root.as_path(),
            HARDENING
                .iter()
                .map(OsString::from)
                .chain(switched_off)
                .chain(arguments.iter().map(OsString::from))
                .collect(),
            self.limits.max_bytes,
        );
        let output = run_command(&spec, &cancel).await.map_err(&broker_error)?;

        if !output.succeeded() {
            // stderr is bounded and reported as a category, not echoed: it can
            // carry absolute paths and remote URLs. One category is named,
            // because a person can act on it: the folder is not a repository
            // at all. git runs with no locale set, so its words are English.
            const NOT_A_REPOSITORY: &[u8] = b"not a git repository";
            if output
                .stderr
                .kept
                .windows(NOT_A_REPOSITORY.len())
                .any(|window| window == NOT_A_REPOSITORY)
            {
                return Err(fail(
                    ErrorCode::ToolFailed,
                    "This folder is not a Git repository".to_owned(),
                ));
            }
            return Err(fail(
                ErrorCode::ToolFailed,
                format!(
                    "git exited unsuccessfully ({})",
                    output
                        .exit_code
                        .map_or_else(|| "signal".to_owned(), |code| code.to_string())
                ),
            ));
        }

        let full_len = output.stdout.total;
        let truncated = output.stdout.truncated();
        let text = String::from_utf8_lossy(&output.stdout.kept).into_owned();

        let lines = text.lines().count();
        Ok(ToolOutcome {
            content: serde_json::json!({
                "tool": request.tool_id(),
                "text": text,
                "byteCount": full_len,
                "truncated": truncated,
            }),
            display_summary: format!(
                "{} returned {lines} line{}",
                request.tool_id(),
                if lines == 1 { "" } else { "s" }
            ),
            side_effect: SideEffect::None,
            truncated,
            changes: Vec::new(),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_runs_exactly_the_arguments_it_declares() {
        // No caller input reaches the command line: the arguments are
        // constants, matched here literally. The broker runs whatever it is
        // given; what makes these three invocations safe is that nothing in
        // them comes from a caller, and this test fails if anyone starts
        // composing them.
        //
        // Spaces inside a single argument are fine — `--pretty=format:%h %ad
        // %s` is one argv element. What must never appear is a *shell*: the
        // arguments are passed as an array to `Command::args`, never joined.
        assert_eq!(
            arguments_for(STATUS),
            Some(
                &[
                    "status",
                    "--porcelain=v1",
                    "--branch",
                    "--ignore-submodules=all"
                ][..]
            )
        );
        assert_eq!(
            arguments_for(DIFF),
            Some(
                &[
                    "--no-pager",
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--ignore-submodules=all",
                    "--stat",
                    "--patch"
                ][..]
            )
        );
        assert_eq!(
            arguments_for(LOG),
            Some(
                &[
                    "--no-pager",
                    "log",
                    "--max-count=20",
                    "--no-color",
                    "--pretty=format:%h %ad %s",
                    "--date=short",
                ][..]
            )
        );
    }

    #[test]
    fn no_argument_could_redirect_output_or_chain_a_command() {
        // Belt and braces. These arguments never reach a shell, so a
        // metacharacter would be inert — but one appearing here would mean
        // someone had started building a command line as text.
        for tool in [STATUS, DIFF, LOG] {
            for argument in arguments_for(tool).expect("known tool") {
                assert!(
                    !argument.contains([';', '|', '&', '>', '<', '`', '$']),
                    "{tool} argument `{argument}` looks like shell syntax"
                );
            }
        }
    }

    #[test]
    fn only_read_only_subcommands_are_offered() {
        let mutating = [
            "add", "commit", "push", "fetch", "pull", "checkout", "reset", "clean", "rm", "mv",
            "merge", "rebase", "config", "remote",
        ];
        for tool in [STATUS, DIFF, LOG] {
            let arguments = arguments_for(tool).expect("known tool");
            for argument in arguments {
                assert!(
                    !mutating.contains(argument),
                    "{tool} must not run the mutating subcommand `{argument}`"
                );
            }
        }
    }

    #[test]
    fn every_invocation_overrides_what_would_run_a_program() {
        // Pinned literally, so dropping one is a visible change here rather
        // than a quiet reopening of `SECURITY_AND_PERMISSIONS.md` §5.
        assert_eq!(
            HARDENING,
            [
                "-c",
                "core.fsmonitor=false",
                "-c",
                "log.showSignature=false",
                "-c",
                HOOKS_OFF,
                "--no-optional-locks"
            ]
        );
        assert!(
            arguments_for(DIFF)
                .expect("known tool")
                .contains(&"--no-textconv")
        );
    }

    #[test]
    fn every_filter_driver_is_found_and_one_that_cannot_be_switched_off_refuses() {
        assert_eq!(filter_drivers(b""), Some(Vec::new()));
        assert_eq!(
            filter_drivers(b"filter.lfs.clean\0filter.lfs.smudge\0filter.a.b.process\0"),
            Some(vec!["lfs".to_owned(), "a.b".to_owned()])
        );
        // `=` would end the `-c` key, leaving the driver armed.
        assert_eq!(filter_drivers(b"filter.a=b.clean\0"), None);
        assert_eq!(filter_drivers(b"filter.a\x07b.clean\0"), None);
        assert_eq!(filter_drivers(b"filter..clean\0"), None);
        let many: Vec<u8> = (0..=MAX_FILTER_DRIVERS)
            .flat_map(|index| format!("filter.driver{index}.clean\0").into_bytes())
            .collect();
        assert_eq!(filter_drivers(&many), None);
    }

    #[test]
    fn listing_the_filters_only_reads_configuration() {
        assert_eq!(
            CONFIGURED_FILTERS,
            [
                "config",
                "--null",
                "--name-only",
                "--get-regexp",
                "^filter\\."
            ]
        );
    }

    #[test]
    fn an_unknown_tool_has_no_arguments() {
        assert!(arguments_for("builtin.git.push").is_none());
        assert!(arguments_for("builtin.fs.read").is_none());
    }
}
