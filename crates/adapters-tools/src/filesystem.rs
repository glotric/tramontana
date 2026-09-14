use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, InvocationId, ProviderId,
    ReplaySafety, RiskClass, SideEffect, ToolDescriptor, content_hash,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};

use crate::identity::FileIdentity;
use crate::limits::{ReadLimits, looks_binary};
use crate::scope::{primary_root, relative_to_grant, resolve_within_grant};

/// Read-only filesystem inspection, scoped to a project's approved roots.
#[derive(Debug, Clone)]
pub struct FilesystemTools {
    limits: ReadLimits,
}

impl Default for FilesystemTools {
    fn default() -> Self {
        Self::new()
    }
}

impl FilesystemTools {
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
}

/// The tool identifiers this provider offers.
pub(crate) const LIST: &str = "builtin.fs.list";
pub(crate) const READ: &str = "builtin.fs.read";
pub(crate) const SEARCH: &str = "builtin.fs.search";
pub(crate) const CREATE: &str = "builtin.fs.create";

/// The largest file the create tool will write.
///
/// A model's arguments are already capped far below this by the broker; the
/// bound is for every other caller.
const MAX_CREATE_BYTES: usize = 1024 * 1024;

fn invalid(request: &AuthorizedToolRequest, message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::InvalidRequest,
        message,
        CorrelationId::new(request.invocation_id().as_str()),
    )
}

fn argument<'a>(request: &'a AuthorizedToolRequest, name: &str) -> Result<&'a str, ErrorEnvelope> {
    request
        .arguments()
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(request, format!("`{name}` must be a string")))
}

fn descriptor(
    tool_id: &str,
    display: &str,
    description: &str,
    schema: serde_json::Value,
) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.to_owned(),
        provider_id: "builtin".to_owned(),
        display_name: display.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        risk_class: RiskClass::Read,
        side_effect: SideEffect::None,
        capability: "filesystem.read".to_owned(),
        supports_cancellation: true,
        replay_safety: ReplaySafety::Safe,
    }
}

/// A tool that changes project files.
///
/// `Modify` and `LocalReversible`: a file this creates can be deleted again,
/// and the permission manager asks before it runs. `safe` to replay only
/// because every write tool verifies the world before acting and reports an
/// effect it finds already made as applied — a claim each one is held to by
/// `idempotent_tool_contract`.
fn write_descriptor(
    tool_id: &str,
    display: &str,
    description: &str,
    schema: serde_json::Value,
) -> ToolDescriptor {
    ToolDescriptor {
        tool_id: tool_id.to_owned(),
        provider_id: "builtin".to_owned(),
        display_name: display.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        risk_class: RiskClass::Modify,
        side_effect: SideEffect::LocalReversible,
        capability: "filesystem.write".to_owned(),
        supports_cancellation: true,
        replay_safety: ReplaySafety::Safe,
    }
}

#[async_trait]
impl ToolProvider for FilesystemTools {
    fn id(&self) -> ProviderId {
        ProviderId::new("builtin")
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        vec![
            descriptor(
                LIST,
                "List directory",
                "List the entries of a directory inside the project.",
                serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false,
                }),
            ),
            descriptor(
                READ,
                "Read file",
                "Read a bounded amount of a text file inside the project.",
                serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false,
                }),
            ),
            descriptor(
                SEARCH,
                "Search text",
                "Find lines matching a literal string inside the project.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "path": { "type": "string" },
                    },
                    "required": ["query"],
                    "additionalProperties": false,
                }),
            ),
            write_descriptor(
                CREATE,
                "Create file",
                "Create a new text file inside the project. Never replaces an existing file: \
                 one with identical content is reported as already created, and any other is \
                 a conflict.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false,
                }),
            ),
        ]
    }

    async fn invoke(
        &self,
        request: AuthorizedToolRequest,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        // Every tool honors cancellation before doing any work, so a cancelled
        // task does not perform one more read on its way out.
        if cancel.is_cancelled() {
            return Err(cancelled(&request));
        }

        let work = async {
            match request.tool_id() {
                LIST => self.list(&request),
                READ => self.read(&request),
                SEARCH => self.search(&request),
                CREATE => self.create(&request, &cancel),
                other => Err(invalid(&request, format!("unknown tool {other}"))),
            }
        };

        tokio::select! {
            result = work => result,
            () = cancel.cancelled() => Err(cancelled(&request)),
            () = tokio::time::sleep(self.limits.deadline) => Err(ErrorEnvelope::new(
                ErrorCode::Timeout,
                "the tool exceeded its deadline",
                CorrelationId::new(request.invocation_id().as_str()),
            )),
        }
    }
}

fn cancelled(request: &AuthorizedToolRequest) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::Cancelled,
        "the tool call was cancelled",
        CorrelationId::new(request.invocation_id().as_str()),
    )
}

impl FilesystemTools {
    fn list(&self, request: &AuthorizedToolRequest) -> Result<ToolOutcome, ErrorEnvelope> {
        let path = resolve_within_grant(request, argument(request, "path")?)?;

        let entries = std::fs::read_dir(&path).map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::NotFound,
                format!("could not list directory: {}", error.kind()),
                CorrelationId::new(request.invocation_id().as_str()),
            )
        })?;

        let mut names: Vec<serde_json::Value> = Vec::new();
        let mut truncated = false;
        for entry in entries.flatten() {
            if names.len() >= self.limits.max_entries {
                truncated = true;
                break;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            names.push(serde_json::json!({
                "name": entry.file_name().to_string_lossy(),
                "kind": if is_dir { "directory" } else { "file" },
            }));
        }
        names.sort_by_key(|entry| entry["name"].as_str().unwrap_or_default().to_owned());

        let shown = names.len();
        Ok(ToolOutcome {
            content: serde_json::json!({ "entries": names }),
            display_summary: format!(
                "Listed {shown} entr{} in {}",
                if shown == 1 { "y" } else { "ies" },
                relative_to_grant(request, &path)
            ),
            side_effect: SideEffect::None,
            truncated,
        })
    }

    fn read(&self, request: &AuthorizedToolRequest) -> Result<ToolOutcome, ErrorEnvelope> {
        let path = resolve_within_grant(request, argument(request, "path")?)?;
        let correlation = CorrelationId::new(request.invocation_id().as_str());

        let metadata = std::fs::metadata(&path).map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::NotFound,
                format!("could not read file: {}", error.kind()),
                correlation.clone(),
            )
        })?;
        if !metadata.is_file() {
            return Err(invalid(request, "path is not a file"));
        }

        let bytes = read_verifying_identity(request, &path)?;
        let relative = relative_to_grant(request, &path);

        // Binary content is described, never returned. Handing it back as text
        // would put escape sequences in an activity stream and noise in a
        // model's context.
        if looks_binary(&bytes) {
            return Ok(ToolOutcome {
                content: serde_json::json!({
                    "path": relative,
                    "binary": true,
                    "byteCount": bytes.len(),
                }),
                display_summary: format!("{relative} is binary ({} bytes)", bytes.len()),
                side_effect: SideEffect::None,
                truncated: false,
            });
        }

        let full_len = bytes.len();
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        let mut truncated = full_len > self.limits.max_bytes;
        if truncated {
            // Cut on a character boundary so the result stays valid text.
            text = text.chars().take(self.limits.max_bytes).collect();
        }

        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > self.limits.max_lines {
            truncated = true;
            text = lines[..self.limits.max_lines].join("\n");
        }

        Ok(ToolOutcome {
            content: serde_json::json!({
                "path": relative,
                "text": text,
                "byteCount": full_len,
                "truncated": truncated,
            }),
            display_summary: if truncated {
                format!("Read {relative} (truncated from {full_len} bytes)")
            } else {
                format!("Read {relative} ({full_len} bytes)")
            },
            side_effect: SideEffect::None,
            truncated,
        })
    }

    fn search(&self, request: &AuthorizedToolRequest) -> Result<ToolOutcome, ErrorEnvelope> {
        let query = argument(request, "query")?;
        if query.is_empty() {
            return Err(invalid(request, "`query` must not be empty"));
        }

        let start = match request
            .arguments()
            .get("path")
            .and_then(serde_json::Value::as_str)
        {
            Some(relative) => resolve_within_grant(request, relative)?,
            None => primary_root(request)?.as_path().to_path_buf(),
        };

        let mut hits = Vec::new();
        let mut truncated = false;
        walk(&start, 0, &mut |file| {
            if hits.len() >= self.limits.max_entries {
                truncated = true;
                return false;
            }
            let Ok(bytes) = std::fs::read(file) else {
                return true;
            };
            if looks_binary(&bytes) {
                return true;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (number, line) in text.lines().enumerate() {
                if !line.contains(query) {
                    continue;
                }
                if hits.len() >= self.limits.max_entries {
                    truncated = true;
                    return false;
                }
                hits.push(serde_json::json!({
                    "path": relative_to_grant(request, file),
                    "line": number + 1,
                    // Bounded so one enormous minified line cannot dominate
                    // the result.
                    "text": line.chars().take(240).collect::<String>(),
                }));
            }
            true
        });

        let found = hits.len();
        Ok(ToolOutcome {
            content: serde_json::json!({ "matches": hits }),
            display_summary: format!("Found {found} match{}", if found == 1 { "" } else { "es" }),
            side_effect: SideEffect::None,
            truncated,
        })
    }
}

/// Directories never searched.
const SKIP: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".venv"];

/// Depth bound, so a deep or looping tree cannot decide how long a search runs.
const MAX_DEPTH: usize = 24;

/// Walk files under `directory`, stopping when `visit` returns `false`.
fn walk(
    directory: &std::path::Path,
    depth: usize,
    visit: &mut impl FnMut(&std::path::Path) -> bool,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    if directory.is_file() {
        visit(directory);
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();

    for path in paths {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if name.as_deref().is_some_and(|n| SKIP.contains(&n)) {
            continue;
        }
        // `symlink_metadata` does not follow links, so a symlinked directory
        // is never descended into and cannot walk the search out of the root.
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            walk(&path, depth + 1, visit);
        } else if metadata.is_file() && !visit(&path) {
            return;
        }
    }
}

/// Read a file, refusing if the path was repointed while we were opening it.
///
/// Containment is checked against a *path*, but the read happens through a
/// *handle*, and the two are resolved at different instants. Between them, a
/// component of the path can be replaced with a symlink pointing anywhere —
/// the time-of-check/time-of-use race `SECURITY_AND_PERMISSIONS.md` §5
/// requires be re-checked.
///
/// Closing it needs three facts to agree:
///
/// 1. the handle we hold has some identity, fixed the moment it opened;
/// 2. the path still resolves inside a granted root *now*;
/// 3. the path still names the same object the handle holds.
///
/// A swap before the open makes (2) fail, because the path now resolves
/// outside. A swap after the open makes (3) fail, because the handle kept the
/// original identity. Either way the read is refused rather than returning
/// content from somewhere nobody approved.
///
/// # Errors
///
/// [`ErrorCode::NotFound`] when the file cannot be opened, and
/// [`ErrorCode::PermissionDenied`] when the checks disagree.
fn read_verifying_identity(
    request: &AuthorizedToolRequest,
    path: &std::path::Path,
) -> Result<Vec<u8>, ErrorEnvelope> {
    use std::io::Read as _;

    let correlation = CorrelationId::new(request.invocation_id().as_str());
    let denied = |message: &str| {
        ErrorEnvelope::new(
            ErrorCode::PermissionDenied,
            message.to_owned(),
            correlation.clone(),
        )
    };

    let opened = std::fs::File::open(path).map_err(|error| {
        ErrorEnvelope::new(
            ErrorCode::NotFound,
            format!("could not read file: {}", error.kind()),
            correlation.clone(),
        )
    })?;

    let Some(mut held) = FileIdentity::of_handle(opened) else {
        // No identity means no way to prove the file did not change. Refusing
        // is the fail-closed answer; proceeding would be a guess.
        return Err(denied("cannot verify the file's identity on this platform"));
    };

    // Re-check containment against the path as it resolves *now*, not as it
    // resolved when the argument was validated.
    resolve_within_grant(request, &request_path(request)?)?;

    let Some(current) = FileIdentity::of_path(path) else {
        return Err(denied("the file disappeared while it was being opened"));
    };
    if current != held {
        return Err(denied(
            "the path changed to a different file while it was being opened",
        ));
    }

    // Read through the verified handle, not by reopening the path.
    let mut bytes = Vec::new();
    held.file_mut().read_to_end(&mut bytes).map_err(|error| {
        ErrorEnvelope::new(
            ErrorCode::ToolFailed,
            format!("could not read file: {}", error.kind()),
            correlation,
        )
    })?;
    Ok(bytes)
}

/// The `path` argument as the caller supplied it.
fn request_path(request: &AuthorizedToolRequest) -> Result<String, ErrorEnvelope> {
    argument(request, "path").map(str::to_owned)
}

impl FilesystemTools {
    /// Create a new text file, atomically, without ever replacing one.
    ///
    /// The whole content goes to a temporary file beside the target, which is
    /// synced and then *hard-linked* into place. A link fails if the target
    /// exists, so this can never overwrite a file, and the target appears
    /// complete or not at all: a crash leaves either nothing or the finished
    /// file, never half of one (`SECURITY_AND_PERMISSIONS.md` §5).
    ///
    /// Safe to replay because it checks before acting. A file already holding
    /// exactly this content is this step, already applied, and is reported
    /// with no side effect; any other file at the path is a conflict. That is
    /// what lets recovery re-invoke an interrupted create without applying it
    /// twice (ADR 0019).
    fn create(
        &self,
        request: &AuthorizedToolRequest,
        cancel: &CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let correlation = CorrelationId::new(request.invocation_id().as_str());
        let fail = |code: ErrorCode, message: String| {
            ErrorEnvelope::new(code, message, correlation.clone())
        };

        let relative = request_path(request)?;
        let content = argument(request, "content")?;
        if content.len() > MAX_CREATE_BYTES {
            return Err(fail(
                ErrorCode::LimitExceeded,
                format!("the content is larger than {MAX_CREATE_BYTES} bytes"),
            ));
        }

        let target = resolve_within_grant(request, &relative)?;
        let display = relative_to_grant(request, &target);
        let parent = target
            .parent()
            .filter(|_| target.file_name().is_some())
            .ok_or_else(|| invalid(request, "the path must name a file"))?;
        // The directory must already exist. Creating directories is a separate
        // effect with its own undo, and this tool does not take it on.
        if !parent.is_dir() {
            return Err(fail(
                ErrorCode::NotFound,
                format!("the directory for {display} does not exist"),
            ));
        }

        match std::fs::symlink_metadata(&target) {
            Ok(existing) => return already_there(request, &target, &display, &existing, content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(fail(
                    ErrorCode::ToolFailed,
                    format!("could not inspect {display}: {}", error.kind()),
                ));
            }
        }

        let temp = parent.join(temp_name(request.invocation_id()));
        // A leftover under this name can only be an earlier attempt of this
        // same invocation, interrupted between writing and linking.
        let _ = std::fs::remove_file(&temp);
        if let Err(error) = write_synced(&temp, content.as_bytes()) {
            let _ = std::fs::remove_file(&temp);
            return Err(fail(
                ErrorCode::ToolFailed,
                format!("could not write {display}: {}", error.kind()),
            ));
        }
        let Some(written) = FileIdentity::of_path(&temp) else {
            let _ = std::fs::remove_file(&temp);
            return Err(fail(
                ErrorCode::PermissionDenied,
                "cannot verify the written file's identity on this platform".to_owned(),
            ));
        };

        // The last point at which cancelling leaves nothing behind.
        if cancel.is_cancelled() {
            let _ = std::fs::remove_file(&temp);
            return Err(cancelled(request));
        }

        let linked = std::fs::hard_link(&temp, &target);
        let _ = std::fs::remove_file(&temp);
        match linked {
            Ok(()) => {}
            // Created by someone else between the check and the link: judge it
            // exactly as a file that had been there all along.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = std::fs::symlink_metadata(&target).map_err(|error| {
                    fail(
                        ErrorCode::ToolFailed,
                        format!("could not inspect {display}: {}", error.kind()),
                    )
                })?;
                return already_there(request, &target, &display, &existing, content);
            }
            Err(error) => {
                return Err(fail(
                    ErrorCode::ToolFailed,
                    format!("could not create {display}: {}", error.kind()),
                ));
            }
        }

        // Re-check after creation (`SECURITY_AND_PERMISSIONS.md` §5): the path
        // must still resolve inside the grant, to the very file just written.
        // If a parent was swapped for a symlink in between, the link may have
        // landed outside the project — so the file is removed, but only when
        // its identity proves it is the one this call wrote.
        let inside = resolve_within_grant(request, &relative).is_ok();
        let ours = FileIdentity::of_path(&target).is_some_and(|now| now == written);
        if !inside || !ours {
            if ours {
                let _ = std::fs::remove_file(&target);
            }
            return Err(fail(
                ErrorCode::PermissionDenied,
                format!("{display} changed while it was being created; the write was not kept"),
            ));
        }
        sync_directory(parent);

        Ok(ToolOutcome {
            content: serde_json::json!({
                "path": display,
                "bytes": content.len(),
                "contentHash": content_hash(content),
                "alreadyApplied": false,
            }),
            display_summary: format!("Created {display} ({} bytes)", content.len()),
            side_effect: SideEffect::LocalReversible,
            truncated: false,
        })
    }
}

/// Judge a path that already exists.
///
/// The same content is this step, already applied: no side effect, and the
/// record says so. Anything else — different content, a directory, a symlink —
/// is a conflict, never an overwrite.
fn already_there(
    request: &AuthorizedToolRequest,
    target: &std::path::Path,
    display: &str,
    existing: &std::fs::Metadata,
    content: &str,
) -> Result<ToolOutcome, ErrorEnvelope> {
    let conflict = |message: String| {
        ErrorEnvelope::new(
            ErrorCode::Conflict,
            message,
            CorrelationId::new(request.invocation_id().as_str()),
        )
    };
    if !existing.file_type().is_file() {
        return Err(conflict(format!(
            "{display} already exists and is not a regular file"
        )));
    }
    // Compared through a verified handle, and only when the length already
    // matches, so a large unrelated file is never read to be refused.
    if existing.len() != content.len() as u64
        || read_verifying_identity(request, target)? != content.as_bytes()
    {
        return Err(conflict(format!(
            "{display} already exists with different content"
        )));
    }
    Ok(ToolOutcome {
        content: serde_json::json!({
            "path": display,
            "bytes": content.len(),
            "contentHash": content_hash(content),
            "alreadyApplied": true,
        }),
        display_summary: format!("{display} already has this content; nothing changed"),
        side_effect: SideEffect::None,
        truncated: false,
    })
}

/// A hidden temporary name, unique to one invocation.
///
/// Derived from the invocation rather than random, so a retry of the same
/// attempt finds — and first removes — its own leftover instead of
/// accumulating them. The target's name is deliberately not part of it: a
/// long name plus a suffix could exceed the platform's limit.
fn temp_name(invocation: &InvocationId) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    invocation.as_str().hash(&mut hasher);
    format!(".tramontana-{:016x}.tmp", hasher.finish())
}

/// Write `bytes` to a new file and flush them to storage.
fn write_synced(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Flush a directory entry to storage, where the platform allows it.
///
/// Makes the new name itself durable, not only the bytes behind it. Best
/// effort: Windows cannot open a directory this way, and a failure here does
/// not undo a write that has already been verified.
fn sync_directory(directory: &std::path::Path) {
    #[cfg(unix)]
    if let Ok(handle) = std::fs::File::open(directory) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}
