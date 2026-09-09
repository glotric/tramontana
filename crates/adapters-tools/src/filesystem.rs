use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tramontana_core::contracts::{
    AuthorizedToolRequest, CorrelationId, ErrorCode, ErrorEnvelope, ProviderId, RiskClass,
    SideEffect, ToolDescriptor,
};
use tramontana_core::ports::{ToolOutcome, ToolProvider};

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

        let bytes = std::fs::read(&path).map_err(|error| {
            ErrorEnvelope::new(
                ErrorCode::NotFound,
                format!("could not read file: {}", error.kind()),
                correlation,
            )
        })?;
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
