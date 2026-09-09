//! Building and validating portable bundles.
//!
//! `PROJECT_FORMAT.md` §5. Both directions are pure functions over records: a
//! bundle is built in memory and validated in memory, so the filesystem
//! adapter that writes and reads one has no policy of its own to get wrong,
//! and every rule here is testable without a temporary directory.
//!
//! Import treats its input as hostile. A bundle arrives from wherever the user
//! got it, and the entry paths, sizes, checksums, and version in its manifest
//! are claims until checked. Nothing in a bundle is ever executed.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::contracts::{
    BUNDLE_MAJOR_VERSION, BUNDLE_MINOR_VERSION, BundleEntry, ConversationRecord, EventEnvelope,
    EventPayload, ExportBundle, ExportManifest, ExportOptions, ExportedConfiguration,
    ImportedRecords, MessagePart, Omission, OmissionCategory, ProjectId, ProjectRecord,
    TaskCheckpoint, TaskId, TaskRecord, Timestamp, Visibility,
};

/// What a redacted value is replaced with.
///
/// A marker rather than a deletion: a reader must be able to tell the
/// difference between "there was a path here and we removed it" and "there was
/// never a path here".
pub const REDACTED_PATH: &str = "[redacted:absolutePath]";
/// What removed tool output is replaced with.
pub const REDACTED_OUTPUT: &str = "[redacted:rawToolOutput]";

/// Bundle-relative file names. Fixed, so a bundle this code produces never
/// contains an interesting path; the validation exists for bundles it did not.
mod files {
    pub(super) const PROJECTS: &str = "projects.ndjson";
    pub(super) const CONFIGURATION: &str = "configuration.ndjson";
    pub(super) const CONVERSATIONS: &str = "conversations.ndjson";
    pub(super) const TASKS: &str = "tasks.ndjson";
    pub(super) const EVENTS: &str = "events.ndjson";
    pub(super) const CHECKPOINTS: &str = "checkpoints.ndjson";
    pub(super) const README: &str = "README.txt";
}

/// Everything an export is assembled from.
///
/// Records rather than repositories: building a bundle is a pure function, and
/// the service that reads storage stays separate from the rules about what may
/// leave the machine.
///
/// No `Default`: a bundle has to be stamped with when it was made, and a
/// default timestamp would be a plausible-looking lie in an exported file.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    /// The project being exported, when the export is scoped to one.
    pub project: Option<ProjectRecord>,
    /// Configuration manifests the exported tasks reference.
    pub configurations: Vec<ExportedConfiguration>,
    /// Conversations to include.
    pub conversations: Vec<ConversationRecord>,
    /// Task rows to include.
    pub tasks: Vec<TaskRecord>,
    /// Durable events for those tasks, in stream order.
    pub events: Vec<EventEnvelope>,
    /// Checkpoints for those tasks.
    pub checkpoints: Vec<TaskCheckpoint>,
    /// What the user chose to include beyond the defaults.
    pub options: ExportOptions,
    /// When the bundle is being produced.
    pub created_at: Timestamp,
}

impl ExportRequest {
    /// An export of nothing, stamped with `created_at`.
    #[must_use]
    pub fn at(created_at: Timestamp) -> Self {
        Self {
            project: None,
            configurations: Vec::new(),
            conversations: Vec::new(),
            tasks: Vec::new(),
            events: Vec::new(),
            checkpoints: Vec::new(),
            options: ExportOptions::default(),
            created_at,
        }
    }
}

/// Counts of what redaction removed, so the manifest can name all of it.
#[derive(Debug, Default)]
struct Omissions {
    secrets: u64,
    absolute_paths: u64,
    raw_tool_output: u64,
    internal_events: u64,
    worker_private_state: u64,
}

impl Omissions {
    fn into_manifest(self) -> Vec<Omission> {
        let mut listed = Vec::new();
        let mut push = |category: OmissionCategory, reason: &str, count: u64| {
            if count > 0 {
                listed.push(Omission {
                    category,
                    reason: reason.to_owned(),
                    count,
                });
            }
        };
        push(
            OmissionCategory::Secret,
            "Secret references are never exported.",
            self.secrets,
        );
        push(
            OmissionCategory::AbsolutePath,
            "Absolute device paths name a machine and often a person; export them deliberately if you need them.",
            self.absolute_paths,
        );
        push(
            OmissionCategory::RawToolOutput,
            "Tool output can contain file contents; only the recorded summary is exported by default.",
            self.raw_tool_output,
        );
        push(
            OmissionCategory::InternalEvent,
            "Internal audit markers are not part of the readable record.",
            self.internal_events,
        );
        push(
            OmissionCategory::WorkerPrivateState,
            "A worker's private continuation data is never exported; canonical state is what carries the work.",
            self.worker_private_state,
        );
        listed
    }
}

/// Build a bundle from records.
#[must_use]
pub fn build_export(request: ExportRequest) -> ExportBundle {
    let mut omissions = Omissions::default();
    let options = request.options;

    let project: Option<ProjectRecord> = request.project.map(|mut project| {
        if !options.include_absolute_paths && !project.roots.is_empty() {
            omissions.absolute_paths += project.roots.len() as u64;
            project.roots = Vec::new();
        }
        project
    });

    let conversations = ndjson(&request.conversations);
    let tasks = ndjson(&request.tasks);

    let events: Vec<EventEnvelope> = request
        .events
        .into_iter()
        .filter_map(|event| redact_event(event, options, &mut omissions))
        .collect();

    let checkpoints: Vec<TaskCheckpoint> = request
        .checkpoints
        .into_iter()
        .map(|mut checkpoint| {
            if let Some(worker) = checkpoint.worker.as_mut()
                && worker.session_state_ref.take().is_some()
            {
                omissions.worker_private_state += 1;
            }
            checkpoint
        })
        .collect();

    let mut files = BTreeMap::new();
    // The project row travels with its roots removed, so importing it
    // re-creates the project without re-granting access to any directory. The
    // user approves a folder again on the new machine, which is the only place
    // that grant can honestly come from (`SECURITY_AND_PERMISSIONS.md` §5).
    files.insert(files::PROJECTS.to_owned(), ndjson(project.as_slice()));
    files.insert(
        files::CONFIGURATION.to_owned(),
        ndjson(&request.configurations),
    );
    files.insert(files::CONVERSATIONS.to_owned(), conversations);
    files.insert(files::TASKS.to_owned(), tasks);
    files.insert(files::EVENTS.to_owned(), ndjson(&events));
    files.insert(files::CHECKPOINTS.to_owned(), ndjson(&checkpoints));

    let task_ids: Vec<TaskId> = request
        .tasks
        .iter()
        .map(|task| task.task_id.clone())
        .collect();
    let listed = omissions.into_manifest();
    files.insert(
        files::README.to_owned(),
        readme(&task_ids, &listed, options),
    );

    let entries = files
        .iter()
        .map(|(path, contents)| BundleEntry {
            path: path.clone(),
            sha256: sha256_hex(contents.as_bytes()),
            byte_count: contents.len() as u64,
        })
        .collect();

    ExportBundle {
        manifest: ExportManifest {
            schema_version: crate::contracts::SCHEMA_VERSION,
            bundle_major_version: BUNDLE_MAJOR_VERSION,
            bundle_minor_version: BUNDLE_MINOR_VERSION,
            created_at: request.created_at,
            project_id: project.as_ref().map(|p| p.project_id.clone()),
            task_ids,
            entries,
            omissions: listed,
            options,
        },
        files,
    }
}

/// Serialize records as newline-delimited JSON.
///
/// A record that cannot be serialized is skipped rather than replaced with a
/// placeholder: a line that is not a record would be a lie the reader has no
/// way to detect.
fn ndjson<T: serde::Serialize>(records: &[T]) -> String {
    let mut document = String::new();
    for record in records {
        if let Ok(line) = serde_json::to_string(record) {
            document.push_str(&line);
            document.push('\n');
        }
    }
    document
}

/// Apply the redaction rules to one event, or drop it.
fn redact_event(
    event: EventEnvelope,
    options: ExportOptions,
    omissions: &mut Omissions,
) -> Option<EventEnvelope> {
    // Secret-visibility events are internal audit markers holding references
    // and hashes; they are not part of the readable record and never leave
    // (`CORE_CONTRACTS.md` §5).
    if matches!(event.visibility, Visibility::Secret) {
        omissions.internal_events += 1;
        return None;
    }

    let mut event = event;
    if !options.include_raw_tool_output
        && let EventPayload::MessageCommitted { message } = &mut event.payload
    {
        for part in &mut message.parts {
            if let MessagePart::ToolResult { content, .. } = part {
                *content = serde_json::Value::String(REDACTED_OUTPUT.to_owned());
                omissions.raw_tool_output += 1;
            }
        }
    }

    if !options.include_absolute_paths {
        let mut payload = match serde_json::to_value(&event.payload) {
            Ok(payload) => payload,
            // An unserializable payload is left exactly as it was rather than
            // exported half-scrubbed.
            Err(_) => return Some(event),
        };
        let removed = scrub_paths(&mut payload);
        if removed > 0
            && let Ok(scrubbed) = serde_json::from_value(payload)
        {
            omissions.absolute_paths += removed;
            event.payload = scrubbed;
        }
    }

    Some(event)
}

/// Replace absolute device paths anywhere in a JSON value.
///
/// Returns how many were replaced. Walking the serialized form rather than
/// matching known fields is deliberate: a path can reach the stream through
/// any string a tool or worker wrote, and a redaction that only covers the
/// fields someone remembered is the kind that fails quietly.
fn scrub_paths(value: &mut serde_json::Value) -> u64 {
    match value {
        serde_json::Value::String(text) => {
            if let Some(replaced) = scrub_paths_in_text(text) {
                *text = replaced;
                1
            } else {
                0
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().map(scrub_paths).sum(),
        serde_json::Value::Object(fields) => fields.values_mut().map(scrub_paths).sum(),
        _ => 0,
    }
}

/// Replace every absolute-path-looking token in `text`, if there are any.
fn scrub_paths_in_text(text: &str) -> Option<String> {
    if !text.split(is_separator).any(looks_absolute) {
        return None;
    }
    let replaced: String = text
        .split_inclusive(is_separator)
        .map(|piece| {
            let (token, tail) = match piece.char_indices().next_back() {
                Some((index, last)) if is_separator(last) => piece.split_at(index),
                _ => (piece, ""),
            };
            if looks_absolute(token) {
                format!("{REDACTED_PATH}{tail}")
            } else {
                piece.to_owned()
            }
        })
        .collect();
    Some(replaced)
}

/// Characters that end a path token in prose.
fn is_separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, ',' | ';' | '"' | '\'' | '(' | ')')
}

/// Whether a token is an absolute path on any supported platform.
///
/// A bare `/` is not: it is a separator in prose far more often than it is a
/// root directory, and redacting it would mangle ordinary text.
fn looks_absolute(token: &str) -> bool {
    let bytes = token.as_bytes();
    match bytes {
        // POSIX absolute, or a Windows UNC share.
        [b'/', rest @ ..] | [b'\\', b'\\', rest @ ..] => !rest.is_empty(),
        // A drive letter, forward or backward slashed.
        [drive, b':', b'\\' | b'/', ..] => drive.is_ascii_alphabetic(),
        _ => false,
    }
}

/// The human-readable note every bundle carries.
fn readme(task_ids: &[TaskId], omissions: &[Omission], options: ExportOptions) -> String {
    let mut text = String::new();
    text.push_str("Tramontana export bundle\n========================\n\n");
    text.push_str(&format!(
        "Bundle format {BUNDLE_MAJOR_VERSION}.{BUNDLE_MINOR_VERSION}, record schema {}.\n",
        crate::contracts::SCHEMA_VERSION
    ));
    text.push_str(&format!("{} task(s) included.\n\n", task_ids.len()));

    text.push_str("Files\n-----\n");
    text.push_str("manifest.json         table of contents, checksums, and omissions\n");
    text.push_str("projects.ndjson       the project, without its device-local roots\n");
    text.push_str("configuration.ndjson  which instructions were in force, by hash\n");
    text.push_str("conversations.ndjson  one conversation per line\n");
    text.push_str("tasks.ndjson          one task per line\n");
    text.push_str("events.ndjson         the durable record, one event per line, in order\n");
    text.push_str("checkpoints.ndjson    resume points, one per line\n\n");

    text.push_str("What is not here\n----------------\n");
    if omissions.is_empty() {
        text.push_str("Nothing was omitted.\n");
    } else {
        for omission in omissions {
            text.push_str(&format!(
                "- {:?} ({}): {}\n",
                omission.category, omission.count, omission.reason
            ));
        }
    }

    text.push_str("\nPrivacy\n-------\n");
    text.push_str("Secret values are never exported. ");
    if options.include_absolute_paths {
        text.push_str(
            "This bundle was exported WITH absolute device paths, which name the machine it came from.\n",
        );
    } else {
        text.push_str("Absolute device paths were redacted.\n");
    }
    if options.include_raw_tool_output {
        text.push_str(
            "It was also exported WITH raw tool output, which can contain file contents.\n",
        );
    }
    text.push_str("\nThis bundle is data. Importing it runs no workflow, tool, hook, or worker.\n");
    text
}

/// Lowercase hex SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    crate::contracts::configuration_hex(&Sha256::digest(bytes))
}

/// Why a bundle was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    /// The bundle layout is from a version this build does not understand.
    #[error("bundle format {found} is not supported; this build reads format {supported}")]
    UnsupportedBundleVersion {
        /// The major version the bundle declared.
        found: u32,
        /// The major version this build reads.
        supported: u32,
    },
    /// An entry path could escape the directory it is unpacked into.
    #[error("entry path {path:?} is not a safe bundle-relative path")]
    UnsafePath {
        /// The rejected path, quoted so an odd one is visible.
        path: String,
    },
    /// The manifest declares a file the bundle does not contain.
    #[error("the manifest declares {path}, which the bundle does not contain")]
    MissingEntry {
        /// The declared path.
        path: String,
    },
    /// The bundle contains a file the manifest does not declare.
    #[error("the bundle contains {path}, which the manifest does not declare")]
    UndeclaredEntry {
        /// The undeclared path.
        path: String,
    },
    /// A file is not the size the manifest claims.
    #[error("{path} is {actual} bytes; the manifest declares {declared}")]
    SizeMismatch {
        /// The path.
        path: String,
        /// The declared size.
        declared: u64,
        /// The actual size.
        actual: u64,
    },
    /// A file, or the bundle, is larger than the reader will accept.
    #[error("{path} is {byte_count} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// The path, or the bundle as a whole.
        path: String,
        /// The size found.
        byte_count: u64,
        /// The limit.
        limit: u64,
    },
    /// A file does not hash to what the manifest claims.
    #[error("{path} does not match its recorded checksum")]
    ChecksumMismatch {
        /// The path.
        path: String,
    },
    /// A file is not the records it is supposed to hold.
    #[error("{path} could not be read: {detail}")]
    Malformed {
        /// The path.
        path: String,
        /// What went wrong, without echoing the content.
        detail: String,
    },
}

/// How much an importer will accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportLimits {
    /// Largest single file.
    pub max_entry_bytes: u64,
    /// Largest bundle in total.
    pub max_total_bytes: u64,
    /// Most files.
    pub max_entries: usize,
}

impl Default for ImportLimits {
    /// Generous for a real export, bounded enough that a hostile one cannot
    /// exhaust memory before it is rejected.
    fn default() -> Self {
        Self {
            max_entry_bytes: 64 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_entries: 64,
        }
    }
}

/// Validate a bundle and recover the records it holds.
///
/// Order matters: version, then paths, then sizes, then checksums, then
/// content. Each step is cheaper than the next and refuses input the next
/// would otherwise have to handle.
///
/// # Errors
///
/// [`ImportError`] describing the first rule the bundle broke.
pub fn validate_import(
    bundle: &ExportBundle,
    limits: ImportLimits,
) -> Result<ImportedRecords, ImportError> {
    if bundle.manifest.bundle_major_version != BUNDLE_MAJOR_VERSION {
        return Err(ImportError::UnsupportedBundleVersion {
            found: bundle.manifest.bundle_major_version,
            supported: BUNDLE_MAJOR_VERSION,
        });
    }

    if bundle.manifest.entries.len() > limits.max_entries {
        return Err(ImportError::TooLarge {
            path: "(bundle)".to_owned(),
            byte_count: bundle.manifest.entries.len() as u64,
            limit: limits.max_entries as u64,
        });
    }

    let mut total = 0u64;
    for entry in &bundle.manifest.entries {
        if !is_safe_relative_path(&entry.path) {
            return Err(ImportError::UnsafePath {
                path: entry.path.clone(),
            });
        }

        let contents = bundle
            .files
            .get(&entry.path)
            .ok_or_else(|| ImportError::MissingEntry {
                path: entry.path.clone(),
            })?;

        let actual = contents.len() as u64;
        if actual > limits.max_entry_bytes {
            return Err(ImportError::TooLarge {
                path: entry.path.clone(),
                byte_count: actual,
                limit: limits.max_entry_bytes,
            });
        }
        if actual != entry.byte_count {
            return Err(ImportError::SizeMismatch {
                path: entry.path.clone(),
                declared: entry.byte_count,
                actual,
            });
        }
        total = total.saturating_add(actual);
        if total > limits.max_total_bytes {
            return Err(ImportError::TooLarge {
                path: "(bundle)".to_owned(),
                byte_count: total,
                limit: limits.max_total_bytes,
            });
        }

        if sha256_hex(contents.as_bytes()) != entry.sha256 {
            return Err(ImportError::ChecksumMismatch {
                path: entry.path.clone(),
            });
        }
    }

    // A file nobody declared has no checksum and no size, so nothing above
    // checked it. Silently unpacking it would defeat every rule above.
    for path in bundle.files.keys() {
        if !bundle
            .manifest
            .entries
            .iter()
            .any(|entry| &entry.path == path)
        {
            return Err(ImportError::UndeclaredEntry { path: path.clone() });
        }
    }

    let mut records = ImportedRecords {
        projects: parse_ndjson(bundle, files::PROJECTS)?,
        configurations: parse_ndjson(bundle, files::CONFIGURATION)?,
        conversations: parse_ndjson(bundle, files::CONVERSATIONS)?,
        tasks: parse_ndjson(bundle, files::TASKS)?,
        events: BTreeMap::new(),
        checkpoints: parse_ndjson(bundle, files::CHECKPOINTS)?,
        quarantined: Vec::new(),
    };

    for event in parse_ndjson::<EventEnvelope>(bundle, files::EVENTS)? {
        records
            .events
            .entry(event.task_id.to_string())
            .or_default()
            .push(event);
    }
    for stream in records.events.values_mut() {
        stream.sort_by_key(|event| event.sequence);
    }

    // Opaque worker state is quarantined rather than restored. It is the one
    // thing in a bundle that a receiving worker could act on directly, and an
    // import must not hand it anything to act on
    // (`PROJECT_FORMAT.md` §5).
    for checkpoint in &mut records.checkpoints {
        if let Some(worker) = checkpoint.worker.as_mut()
            && let Some(reference) = worker.session_state_ref.take()
        {
            records.quarantined.push(reference);
        }
    }

    Ok(records)
}

/// Read one bundle file as newline-delimited records.
fn parse_ndjson<T: serde::de::DeserializeOwned>(
    bundle: &ExportBundle,
    path: &str,
) -> Result<Vec<T>, ImportError> {
    let Some(contents) = bundle.files.get(path) else {
        // A bundle may legitimately omit a file it has no records for.
        return Ok(Vec::new());
    };

    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| ImportError::Malformed {
                path: path.to_owned(),
                // The line number and the parse failure, never the line: a
                // malformed bundle's content is exactly what should not be
                // echoed into a log.
                detail: format!("line {}: {}", index + 1, error),
            })
        })
        .collect()
}

/// Whether a path may be joined onto a destination directory.
///
/// Rejects absolute paths, drive letters, UNC prefixes, `..` and `.`
/// components, empty components, backslashes, and control characters. The
/// question is not whether the path is unusual but whether joining it could
/// ever address something outside the directory it belongs in.
#[must_use]
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 512 {
        return false;
    }
    if path.contains('\\') || path.contains('\0') {
        return false;
    }
    if path.chars().any(char::is_control) {
        return false;
    }
    if path.starts_with('/') {
        return false;
    }
    // A drive-relative path such as `C:file` is absolute on the volume.
    if path.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    path.split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// The project a bundle belongs to, for a caller staging an import.
#[must_use]
pub fn imported_project_id(bundle: &ExportBundle) -> Option<ProjectId> {
    bundle.manifest.project_id.clone()
}
