//! Portable export bundles and worker handoff.
//!
//! `PROJECT_FORMAT.md` §5 is normative for the bundle, §4 for handoff. The
//! rules that outlive the encoding are the ones this module makes structural:
//!
//! - **Export is a data-release action.** Secrets are excluded unconditionally
//!   and are not an option a caller can turn back on. Absolute paths, raw tool
//!   output, and artifacts are excluded *by default* and can be included only
//!   deliberately.
//! - **Everything dropped is named.** An omission the reader cannot see is
//!   indistinguishable from data that never existed, so every exclusion is
//!   counted in [`ExportManifest::omissions`].
//! - **Import is untrusted input.** Entry paths, sizes, checksums, and schema
//!   versions are all validated before a record is believed, and nothing in a
//!   bundle is ever executed.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    AgentDescriptor, ConversationId, ConversationRecord, EffectiveConfiguration, EventEnvelope,
    PendingAction, ProjectId, ProjectRecord, SCHEMA_VERSION, TaskCheckpoint, TaskId, TaskRecord,
    Timestamp,
};

/// Major version of the bundle layout.
///
/// Separate from [`SCHEMA_VERSION`], which versions the records *inside* the
/// bundle. A reader that does not recognize this refuses the whole bundle
/// rather than guessing at its shape (`PROJECT_FORMAT.md` §5).
pub const BUNDLE_MAJOR_VERSION: u32 = 1;

/// Minor version, incremented for additive changes a reader may ignore.
pub const BUNDLE_MINOR_VERSION: u32 = 0;

/// What kind of data was left out of an export.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum OmissionCategory {
    /// A secret reference or credential-store value. Never included.
    Secret,
    /// An absolute device path, which names a machine and often a person.
    AbsolutePath,
    /// Unredacted output a tool produced.
    RawToolOutput,
    /// A file an invocation produced.
    Artifact,
    /// An internal audit marker never shown outside the application.
    InternalEvent,
    /// A worker's private continuation data.
    WorkerPrivateState,
}

impl OmissionCategory {
    /// Whether the user may choose to include this category.
    ///
    /// Secrets and worker-private state are not choices. Everything else is a
    /// default the user can override for a bundle they intend to keep.
    #[must_use]
    pub fn is_optional(self) -> bool {
        !matches!(self, Self::Secret | Self::WorkerPrivateState)
    }
}

/// One thing an export left out, and how much of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Omission {
    /// What kind of data.
    pub category: OmissionCategory,
    /// Why it was left out, in a sentence a person can act on.
    pub reason: String,
    /// How many values were affected.
    pub count: u64,
}

/// One file in a bundle, with what a reader needs to trust it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BundleEntry {
    /// Bundle-relative path. Validated on import before it is used.
    pub path: String,
    /// Lowercase hex SHA-256 of the contents.
    pub sha256: String,
    /// Size in bytes, checked against the contents before the checksum.
    pub byte_count: u64,
}

/// What an export chose to include.
///
/// Absent means the default, which is to exclude. The flags exist so a bundle
/// records the decision that produced it rather than leaving a reader to infer
/// it from what happens to be present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    /// Keep absolute device paths instead of redacting them.
    #[serde(default)]
    pub include_absolute_paths: bool,
    /// Keep unredacted tool output.
    #[serde(default)]
    pub include_raw_tool_output: bool,
    /// Include artifact files.
    #[serde(default)]
    pub include_artifacts: bool,
}

/// The bundle's table of contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExportManifest {
    /// Version of the records inside the bundle.
    pub schema_version: u32,
    /// Major version of the bundle layout. Refused when unrecognized.
    pub bundle_major_version: u32,
    /// Minor version. A reader may ignore what it does not know.
    pub bundle_minor_version: u32,
    /// When the bundle was produced.
    pub created_at: Timestamp,
    /// The project, when the export was scoped to one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<ProjectId>,
    /// Tasks included, in export order.
    pub task_ids: Vec<TaskId>,
    /// Every file in the bundle.
    pub entries: Vec<BundleEntry>,
    /// What was left out, and how much.
    pub omissions: Vec<Omission>,
    /// What the export chose to include.
    pub options: ExportOptions,
}

/// A bundle held in memory: its manifest and every file it declares.
///
/// Deliberately not a directory. Building and validating a bundle is pure, so
/// both are testable without touching a filesystem, and the adapter that does
/// touch one has no policy of its own to get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportBundle {
    /// The table of contents.
    pub manifest: ExportManifest,
    /// Bundle-relative path to file contents.
    pub files: BTreeMap<String, String>,
}

/// A configuration manifest with the project it belongs to.
///
/// The manifest itself does not name its project — it is stored under one —
/// so a bundle has to carry the pairing. Without it an imported task would
/// reference a manifest nothing could resolve, and the record could not say
/// which instructions were in force when the work happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExportedConfiguration {
    /// The project this manifest was resolved for.
    pub project_id: ProjectId,
    /// The secret-free manifest. Instruction bodies are not copied.
    pub configuration: EffectiveConfiguration,
}

/// The records an import recovered, after validation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportedRecords {
    /// Projects in the bundle, with their device-local roots already removed.
    pub projects: Vec<ProjectRecord>,
    /// Configuration manifests the tasks reference.
    pub configurations: Vec<ExportedConfiguration>,
    /// Conversations in the bundle.
    pub conversations: Vec<ConversationRecord>,
    /// Task rows in the bundle.
    pub tasks: Vec<TaskRecord>,
    /// Durable events, grouped by the task they belong to.
    pub events: BTreeMap<String, Vec<EventEnvelope>>,
    /// Checkpoints in the bundle.
    pub checkpoints: Vec<TaskCheckpoint>,
    /// Opaque worker state that was quarantined rather than restored.
    pub quarantined: Vec<String>,
}

/// One fact in a handoff, normalized so any worker can read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HandoffEntry {
    /// Position in the task's durable stream.
    pub sequence: u64,
    /// When it happened.
    pub occurred_at: Timestamp,
    /// What kind of fact this is.
    pub kind: String,
    /// A secret-free description.
    pub summary: String,
}

/// Everything a different worker needs to continue a task.
///
/// Assembled from canonical records only. A worker's opaque continuation data
/// is never read to build this, which is what makes the claim in
/// `PROJECT_FORMAT.md` §4 — that canonical state is sufficient — testable
/// rather than aspirational.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HandoffBundle {
    /// Contract schema version.
    pub schema_version: u32,
    /// The task being handed over.
    pub task_id: TaskId,
    /// Its project, when it has one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<ProjectId>,
    /// Its conversation, when it has one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub conversation_id: Option<ConversationId>,
    /// What the user asked for.
    pub goal: String,
    /// When the handoff was assembled.
    pub created_at: Timestamp,
    /// The worker that had the task, for provenance only.
    ///
    /// A receiving worker is told who held it before, not asked to imitate
    /// them. Nothing private to that worker is carried.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub previous_worker: Option<AgentDescriptor>,
    /// The point being handed over.
    pub event_sequence: u64,
    /// The task's normalized history.
    pub history: Vec<HandoffEntry>,
    /// Steps already done, which the receiving worker must not repeat.
    pub completed_step_ids: Vec<String>,
    /// What was in flight, when anything was.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pending_action: Option<PendingAction>,
    /// The effective configuration in force, for a project task.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub configuration_manifest_hash: Option<String>,
    /// What this handoff does not carry.
    pub omissions: Vec<Omission>,
}

impl HandoffBundle {
    /// Build an empty handoff for `task_id` at the current schema version.
    #[must_use]
    pub fn new(task_id: TaskId, goal: impl Into<String>, created_at: Timestamp) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            task_id,
            project_id: None,
            conversation_id: None,
            goal: goal.into(),
            created_at,
            previous_worker: None,
            event_sequence: 0,
            history: Vec::new(),
            completed_step_ids: Vec::new(),
            pending_action: None,
            configuration_manifest_hash: None,
            omissions: Vec::new(),
        }
    }
}
