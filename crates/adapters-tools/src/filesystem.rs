use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, InvocationId, ProviderId,
    ReplaySafety, RiskClass, SideEffect, ToolDescriptor, content_hash, content_hash_bytes,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};

use crate::identity::FileIdentity;
use crate::limits::{ReadLimits, looks_binary};
use crate::scope::{primary_root, relative_to_grant, resolve_within_grant, same_root};

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
pub(crate) const PATCH: &str = "builtin.fs.patch";
pub(crate) const MOVE: &str = "builtin.fs.move";

/// The largest file the patch tool will edit, before or after the edit.
const MAX_PATCH_BYTES: usize = 1024 * 1024;

/// How many places a replay check will examine.
///
/// Verifying that an edit was already applied means reversing it at each
/// place its replacement text occurs and hashing the result. Past this many
/// places that costs more than it is worth, so the tool reports a conflict
/// rather than claim an effect it could not afford to verify.
const MAX_REPLAY_CANDIDATES: usize = 64;

/// The largest file the create tool will write.
///
/// A model's arguments are already capped far below this by the broker; the
/// bound is for every other caller.
const MAX_CREATE_BYTES: usize = 1024 * 1024;

/// The largest file the move tool will move.
///
/// Proving that the file is the one the caller read means hashing it, and so
/// reading it; this bounds that read.
const MAX_MOVE_BYTES: u64 = 64 * 1024 * 1024;

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
            write_descriptor(
                PATCH,
                "Patch file",
                "Replace one exact occurrence of `oldText` with `newText` in an existing text \
                 file. `expectedHash` must be the `contentHash` a read returned for the file: the \
                 edit is refused if the file has changed since. `oldText` must occur exactly \
                 once; include surrounding lines to make it unique, or to delete text.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "expectedHash": { "type": "string" },
                        "oldText": { "type": "string" },
                        "newText": { "type": "string" },
                    },
                    "required": ["path", "expectedHash", "oldText", "newText"],
                    "additionalProperties": false,
                }),
            ),
            write_descriptor(
                MOVE,
                "Move file",
                "Move or rename a file within one project folder. `expectedHash` must be the \
                 `contentHash` a read returned for the file: the move is refused if the file has \
                 changed since. Never replaces an existing file, and the destination's directory \
                 must already exist.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "from": { "type": "string" },
                        "to": { "type": "string" },
                        "expectedHash": { "type": "string" },
                    },
                    "required": ["from", "to", "expectedHash"],
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
                PATCH => self.patch(&request, &cancel),
                MOVE => self.move_file(&request, &cancel),
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
                    "contentHash": content_hash_bytes(&bytes),
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
                // Of the whole file as it is on disk, even when the text above
                // is cut short: it is the precondition a patch checks, so it
                // must describe exactly the bytes a patch would replace.
                "contentHash": content_hash_bytes(&bytes),
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
fn read_verified_at(
    request: &AuthorizedToolRequest,
    relative: &str,
    path: &std::path::Path,
) -> Result<(Vec<u8>, FileIdentity), ErrorEnvelope> {
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
    resolve_within_grant(request, relative)?;

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
    Ok((bytes, held))
}

/// [`read_verified_at`], for the file the `path` argument names.
fn read_verified(
    request: &AuthorizedToolRequest,
    path: &std::path::Path,
) -> Result<(Vec<u8>, FileIdentity), ErrorEnvelope> {
    read_verified_at(request, &request_path(request)?, path)
}

/// [`read_verified`], for callers that need only the bytes.
fn read_verifying_identity(
    request: &AuthorizedToolRequest,
    path: &std::path::Path,
) -> Result<Vec<u8>, ErrorEnvelope> {
    read_verified(request, path).map(|(bytes, _)| bytes)
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

impl FilesystemTools {
    /// Replace one exact occurrence of text in an existing file, atomically.
    ///
    /// The precondition is a hash of the whole file as the caller last read it
    /// — `expectedHash`, from the read tool's `contentHash`. A file that has
    /// changed since is never edited on the strength of an old reading.
    ///
    /// Safe to replay because a changed hash is examined before it is refused.
    /// If the file is exactly what this edit would have made of the expected
    /// original — reversing it at one of the places its replacement occurs
    /// gives back that original's hash — the step was already applied and is
    /// reported with no side effect. Anything else is a conflict (ADR 0020).
    fn patch(
        &self,
        request: &AuthorizedToolRequest,
        cancel: &CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let correlation = CorrelationId::new(request.invocation_id().as_str());
        let fail = |code: ErrorCode, message: String| {
            ErrorEnvelope::new(code, message, correlation.clone())
        };

        let relative = request_path(request)?;
        let expected = argument(request, "expectedHash")?;
        let old_text = argument(request, "oldText")?;
        let new_text = argument(request, "newText")?;
        if !valid_hash(expected) {
            return Err(invalid(
                request,
                "`expectedHash` must be the 64-character lowercase `contentHash` a read returned",
            ));
        }
        // Both non-empty: an empty replacement would make every byte position a
        // candidate when verifying a replay. A deletion names its context.
        if old_text.is_empty() || new_text.is_empty() {
            return Err(invalid(
                request,
                "`oldText` and `newText` must both be non-empty; to delete text, include \
                 the lines around it in both",
            ));
        }
        if old_text == new_text {
            return Err(invalid(
                request,
                "`oldText` and `newText` are identical, so there is nothing to change",
            ));
        }

        let target = resolve_within_grant(request, &relative)?;
        let display = relative_to_grant(request, &target);
        let metadata = std::fs::symlink_metadata(&target).map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::NotFound {
                ErrorCode::NotFound
            } else {
                ErrorCode::ToolFailed
            };
            fail(
                code,
                format!("could not inspect {display}: {}", error.kind()),
            )
        })?;
        // Never through a symlink, and never anything but a regular file.
        if !metadata.file_type().is_file() {
            return Err(fail(
                ErrorCode::Conflict,
                format!("{display} is not a regular file"),
            ));
        }
        if metadata.len() > MAX_PATCH_BYTES as u64 {
            return Err(fail(
                ErrorCode::LimitExceeded,
                format!("{display} is larger than {MAX_PATCH_BYTES} bytes"),
            ));
        }

        let (bytes, held) = read_verified(request, &target)?;
        if looks_binary(&bytes) {
            return Err(invalid(request, format!("{display} is binary, not text")));
        }
        let Ok(current) = std::str::from_utf8(&bytes) else {
            return Err(invalid(
                request,
                format!("{display} is not valid UTF-8 text"),
            ));
        };
        let current_hash = content_hash_bytes(&bytes);

        if current_hash != expected {
            // Not the file the caller read. Either this edit was already
            // applied — a replay after an interruption — or something else
            // changed it, and only the first is proven by the hashes.
            return if already_applied(current, old_text, new_text, expected) {
                Ok(ToolOutcome {
                    content: serde_json::json!({
                        "path": display,
                        "contentHash": current_hash,
                        "alreadyApplied": true,
                    }),
                    display_summary: format!("{display} already has this edit; nothing changed"),
                    side_effect: SideEffect::None,
                    truncated: false,
                })
            } else {
                Err(fail(
                    ErrorCode::Conflict,
                    format!(
                        "{display} has changed since it was read; read it again before editing"
                    ),
                ))
            };
        }

        let occurrences = current.matches(old_text).count();
        if occurrences != 1 {
            return Err(invalid(
                request,
                if occurrences == 0 {
                    format!("`oldText` does not occur in {display}")
                } else {
                    format!(
                        "`oldText` occurs {occurrences} times in {display}; include more \
                         surrounding text so it names one place"
                    )
                },
            ));
        }
        let at = current.find(old_text).unwrap_or_default();
        let updated = format!(
            "{}{new_text}{}",
            &current[..at],
            &current[at + old_text.len()..]
        );
        if updated.len() > MAX_PATCH_BYTES {
            return Err(fail(
                ErrorCode::LimitExceeded,
                format!("the edited {display} would be larger than {MAX_PATCH_BYTES} bytes"),
            ));
        }
        let line = current[..at].matches('\n').count() + 1;

        let parent = target
            .parent()
            .ok_or_else(|| invalid(request, "the path must name a file"))?;
        let temp = parent.join(temp_name(request.invocation_id()));
        let _ = std::fs::remove_file(&temp);
        let prepared = write_synced(&temp, updated.as_bytes())
            // The replacement is a new file and would get default permissions.
            // Keeping the original's is deliberate (`SECURITY_AND_PERMISSIONS.md` §5).
            .and_then(|()| std::fs::set_permissions(&temp, metadata.permissions()));
        if let Err(error) = prepared {
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

        // Immediately before replacing: the path must still resolve inside the
        // grant, to the very file that was read, still holding the bytes that
        // were hashed. An edit computed from an old reading must not land on
        // top of a file that moved or changed in the meantime.
        let unchanged = resolve_within_grant(request, &relative).is_ok()
            && FileIdentity::of_path(&target).is_some_and(|now| now == held)
            && std::fs::read(&target).is_ok_and(|now| content_hash_bytes(&now) == expected);
        // Released before the rename: Windows will not replace a file that is
        // still held open.
        drop(held);
        if !unchanged {
            let _ = std::fs::remove_file(&temp);
            return Err(fail(
                ErrorCode::Conflict,
                format!("{display} changed while the edit was being prepared; nothing was written"),
            ));
        }

        if let Err(error) = std::fs::rename(&temp, &target) {
            let _ = std::fs::remove_file(&temp);
            return Err(fail(
                ErrorCode::ToolFailed,
                format!("could not replace {display}: {}", error.kind()),
            ));
        }

        // After replacing: the path must resolve inside the grant, to the file
        // just written (`SECURITY_AND_PERMISSIONS.md` §5).
        let inside = resolve_within_grant(request, &relative).is_ok();
        let ours = FileIdentity::of_path(&target).is_some_and(|now| now == written);
        if !inside || !ours {
            return Err(fail(
                ErrorCode::PermissionDenied,
                format!(
                    "{display} changed while it was being replaced; the result could not be verified"
                ),
            ));
        }
        sync_directory(parent);

        Ok(ToolOutcome {
            content: serde_json::json!({
                "path": display,
                "line": line,
                "bytesBefore": bytes.len(),
                "bytesAfter": updated.len(),
                "previousHash": expected,
                "contentHash": content_hash_bytes(updated.as_bytes()),
                "alreadyApplied": false,
            }),
            display_summary: format!("Patched {display} at line {line}"),
            side_effect: SideEffect::LocalReversible,
            truncated: false,
        })
    }
}

/// Whether `current` is exactly what one application of this edit would have
/// made of the file whose hash is `expected`.
///
/// Reverses the edit at each place `new_text` occurs — overlapping places
/// included, since the true one may overlap another — and compares hashes. So
/// "already applied" rests on the original's hash, not on the replacement
/// merely appearing somewhere. The reconstructed original must also satisfy
/// the edit's own precondition, `old_text` exactly once, or this edit could
/// never have produced `current`. Past `MAX_REPLAY_CANDIDATES` places the
/// answer is no: an unverifiable claim is not made.
fn already_applied(current: &str, old_text: &str, new_text: &str, expected: &str) -> bool {
    let mut places = Vec::new();
    let mut from = 0;
    while let Some(found) = current[from..].find(new_text) {
        let at = from + found;
        places.push(at);
        if places.len() > MAX_REPLAY_CANDIDATES {
            return false;
        }
        // One character on, not one match on, so overlapping places are seen.
        from = at + current[at..].chars().next().map_or(1, char::len_utf8);
    }
    places.into_iter().any(|at| {
        let original = format!(
            "{}{old_text}{}",
            &current[..at],
            &current[at + new_text.len()..]
        );
        content_hash_bytes(original.as_bytes()) == expected
            && original.matches(old_text).count() == 1
    })
}

/// Whether `value` has the shape of a `contentHash`: 64 lowercase hex digits.
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl FilesystemTools {
    /// Move or rename one regular file within one approved root, never
    /// replacing anything.
    ///
    /// The file is hard-linked under its new name and only then unlinked from
    /// its old one. A link fails if the new name exists, so a move cannot
    /// overwrite a file, and the file always has a name: an interruption leaves
    /// it under the old one, the new one, or both — never neither (ADR 0022).
    ///
    /// The precondition is the file's hash as the caller read it, as for a
    /// patch. It is also what makes a replay provable: with the old name gone
    /// and the new one holding exactly those bytes, the step was already
    /// applied and is reported with no side effect. A file found under both
    /// names is a conflict rather than something to finish, because where a
    /// file system ignores letter case, two spellings can reach one entry, and
    /// removing the "old" one would remove the file.
    fn move_file(
        &self,
        request: &AuthorizedToolRequest,
        cancel: &CancellationToken,
    ) -> Result<ToolOutcome, ErrorEnvelope> {
        let correlation = CorrelationId::new(request.invocation_id().as_str());
        let fail = |code: ErrorCode, message: String| {
            ErrorEnvelope::new(code, message, correlation.clone())
        };

        let from = argument(request, "from")?;
        let to = argument(request, "to")?;
        let expected = argument(request, "expectedHash")?;
        if !valid_hash(expected) {
            return Err(invalid(
                request,
                "`expectedHash` must be the 64-character lowercase `contentHash` a read returned",
            ));
        }

        let source = resolve_within_grant(request, from)?;
        let target = resolve_within_grant(request, to)?;
        if source.file_name().is_none() || target.file_name().is_none() {
            return Err(invalid(request, "`from` and `to` must each name a file"));
        }
        let from_display = relative_to_grant(request, &source);
        let to_display = relative_to_grant(request, &target);
        // Within one approved root (`BUILD_ORDER.md` §16): a move is never a
        // way to carry a file from one approved folder into another.
        if !same_root(request, &source, &target) {
            return Err(fail(
                ErrorCode::PermissionDenied,
                format!(
                    "{from_display} and {to_display} are in different approved folders; a move \
                     stays within one"
                ),
            ));
        }

        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return moved_already(request, to, &target, &from_display, &to_display, expected);
            }
            Err(error) => {
                return Err(fail(
                    ErrorCode::ToolFailed,
                    format!("could not inspect {from_display}: {}", error.kind()),
                ));
            }
        };
        // Never a symlink or a directory: only the file itself is moved.
        if !metadata.file_type().is_file() {
            return Err(fail(
                ErrorCode::Conflict,
                format!("{from_display} is not a regular file; only files are moved"),
            ));
        }
        if metadata.len() > MAX_MOVE_BYTES {
            return Err(fail(
                ErrorCode::LimitExceeded,
                format!("{from_display} is larger than {MAX_MOVE_BYTES} bytes"),
            ));
        }
        let (bytes, held) = read_verified_at(request, from, &source)?;
        if content_hash_bytes(&bytes) != expected {
            return Err(fail(
                ErrorCode::Conflict,
                format!(
                    "{from_display} has changed since it was read; read it again before moving it"
                ),
            ));
        }
        drop(bytes);

        let parent = target
            .parent()
            .ok_or_else(|| invalid(request, "`to` must name a file"))?;
        // The directory must already exist, as for a create.
        if !parent.is_dir() {
            return Err(fail(
                ErrorCode::NotFound,
                format!("the directory for {to_display} does not exist"),
            ));
        }
        // Looked at first only for a clearer answer. The link below is what
        // actually refuses to replace, including a file created meanwhile.
        if std::fs::symlink_metadata(&target).is_ok() {
            return Err(occupied(
                request,
                &target,
                &held,
                &from_display,
                &to_display,
            ));
        }

        // The last point at which cancelling leaves nothing behind.
        if cancel.is_cancelled() {
            return Err(cancelled(request));
        }

        match std::fs::hard_link(&source, &target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(occupied(
                    request,
                    &target,
                    &held,
                    &from_display,
                    &to_display,
                ));
            }
            Err(error) => {
                return Err(fail(
                    ErrorCode::ToolFailed,
                    format!("could not move {from_display}: {}", error.kind()),
                ));
            }
        }

        // After linking (`SECURITY_AND_PERMISSIONS.md` §5): the new name must
        // resolve inside the grant, to the very file that was read. A link
        // that proves to be ours is taken back; anything else is left for a
        // person, since removing it could remove someone else's file.
        let inside = resolve_within_grant(request, to).is_ok();
        let ours = FileIdentity::of_path(&target).is_some_and(|now| now == held);
        if !inside || !ours {
            if ours {
                let _ = std::fs::remove_file(&target);
            }
            return Err(fail(
                ErrorCode::PermissionDenied,
                format!(
                    "{to_display} could not be verified after linking; {from_display} was not \
                     removed"
                ),
            ));
        }

        // Before unlinking: the old name must still resolve inside the grant,
        // to the same file. If it names something else now, removing it would
        // delete a file nobody asked to move.
        let unchanged = resolve_within_grant(request, from).is_ok()
            && FileIdentity::of_path(&source).is_some_and(|now| now == held);
        // Released before the removal, which Windows may refuse while the
        // file is held open.
        drop(held);
        if !unchanged {
            return Err(fail(
                ErrorCode::Conflict,
                format!(
                    "{from_display} changed during the move; {to_display} now holds the file that \
                     was read, and {from_display} was left alone"
                ),
            ));
        }
        if let Err(error) = std::fs::remove_file(&source) {
            return Err(fail(
                ErrorCode::ToolFailed,
                format!(
                    "{to_display} now holds the file, but {from_display} could not be removed: {}",
                    error.kind()
                ),
            ));
        }
        sync_directory(parent);
        if let Some(previous) = source.parent() {
            sync_directory(previous);
        }

        Ok(ToolOutcome {
            content: serde_json::json!({
                "from": from_display,
                "to": to_display,
                "contentHash": expected,
                "alreadyApplied": false,
            }),
            display_summary: format!("Moved {from_display} to {to_display}"),
            side_effect: SideEffect::LocalReversible,
            truncated: false,
        })
    }
}

/// Refuse a destination that already exists, saying which refusal it is.
///
/// The same file under both names is either an interrupted move or two
/// spellings of one entry, and the two cannot be told apart portably, so
/// neither is "finished" by removing a name.
fn occupied(
    request: &AuthorizedToolRequest,
    target: &std::path::Path,
    held: &FileIdentity,
    from_display: &str,
    to_display: &str,
) -> ErrorEnvelope {
    let same = FileIdentity::of_path(target).is_some_and(|now| now == *held);
    ErrorEnvelope::new(
        ErrorCode::Conflict,
        if same {
            format!(
                "{to_display} already names the same file as {from_display}. Either both names \
                 exist, as after an interrupted move, and removing one finishes or undoes it; or \
                 this file system treats the two names as one, as it does for names differing \
                 only in letter case. Nothing was changed"
            )
        } else {
            format!("{to_display} already exists; a move never replaces a file")
        },
        CorrelationId::new(request.invocation_id().as_str()),
    )
}

/// Judge a move whose source is gone.
///
/// Already applied only when the destination is a regular file holding exactly
/// the bytes the caller read. A destination holding anything else was not made
/// by this step, and saying otherwise would be a claim nobody verified.
fn moved_already(
    request: &AuthorizedToolRequest,
    to: &str,
    target: &std::path::Path,
    from_display: &str,
    to_display: &str,
    expected: &str,
) -> Result<ToolOutcome, ErrorEnvelope> {
    let correlation = CorrelationId::new(request.invocation_id().as_str());
    let Ok(metadata) = std::fs::symlink_metadata(target) else {
        return Err(ErrorEnvelope::new(
            ErrorCode::NotFound,
            format!("{from_display} does not exist"),
            correlation,
        ));
    };
    let conflict = || {
        ErrorEnvelope::new(
            ErrorCode::Conflict,
            format!(
                "{from_display} does not exist, and {to_display} is not the file that was read"
            ),
            correlation.clone(),
        )
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_MOVE_BYTES {
        return Err(conflict());
    }
    let (bytes, _) = read_verified_at(request, to, target)?;
    if content_hash_bytes(&bytes) != expected {
        return Err(conflict());
    }
    Ok(ToolOutcome {
        content: serde_json::json!({
            "from": from_display,
            "to": to_display,
            "contentHash": expected,
            "alreadyApplied": true,
        }),
        display_summary: format!("{to_display} already holds this file; nothing changed"),
        side_effect: SideEffect::None,
        truncated: false,
    })
}
