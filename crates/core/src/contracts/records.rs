//! Durable records of the local operational plane.
//!
//! `PROJECT_FORMAT.md` §3 lists the provider-neutral records the format must
//! version. These are the ones M2 persists; tasks' events live in their own
//! stream, and checkpoints arrive with M4.
//!
//! Paths appear here as *local bindings*. An exported bundle redacts them by
//! default, because an absolute path names a person's machine and often their
//! name (`PROJECT_FORMAT.md` §3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ConversationId, ProjectId, SCHEMA_VERSION, TaskId, TaskState, Timestamp};

/// A project and the roots the user approved for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier.
    pub project_id: ProjectId,
    /// Display name.
    pub name: String,
    /// Canonical approved roots. Device-local bindings, redacted on export.
    pub roots: Vec<String>,
    /// When the project was created.
    pub created_at: Timestamp,
}

impl ProjectRecord {
    /// Build a record with the current schema version.
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        name: impl Into<String>,
        roots: Vec<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            project_id,
            name: name.into(),
            roots,
            created_at,
        }
    }
}

/// A conversation, optionally scoped to a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRecord {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier.
    pub conversation_id: ConversationId,
    /// Project this belongs to. Absent means projectless, which carries no
    /// implicit filesystem access (`PHASE_1_ACCEPTANCE_CRITERIA.md` §8).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<ProjectId>,
    /// Display title.
    pub title: String,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it was archived, if it has been.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub archived_at: Option<Timestamp>,
}

impl ConversationRecord {
    /// Build a record with the current schema version.
    #[must_use]
    pub fn new(
        conversation_id: ConversationId,
        project_id: Option<ProjectId>,
        title: impl Into<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            conversation_id,
            project_id,
            title: title.into(),
            created_at,
            archived_at: None,
        }
    }
}

/// A task row.
///
/// `state` and `last_sequence` are a cache of the event stream, not a second
/// source of truth. After an unclean stop they are rebuilt from the events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier.
    pub task_id: TaskId,
    /// Conversation this belongs to, when it has one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub conversation_id: Option<ConversationId>,
    /// Project this belongs to, when it has one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<ProjectId>,
    /// Secret-free summary of what was asked for.
    pub goal_summary: String,
    /// Effective-configuration manifest captured before this task started.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub configuration_manifest_hash: Option<String>,
    /// Cached state, projected from the events.
    pub state: TaskState,
    /// Highest durable sequence written.
    pub last_sequence: u64,
    /// When the task was created.
    pub created_at: Timestamp,
    /// When the projection was last updated.
    pub updated_at: Timestamp,
}

impl TaskRecord {
    /// Build a freshly created task.
    #[must_use]
    pub fn new(
        task_id: TaskId,
        conversation_id: Option<ConversationId>,
        project_id: Option<ProjectId>,
        goal_summary: impl Into<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            task_id,
            conversation_id,
            project_id,
            goal_summary: goal_summary.into(),
            configuration_manifest_hash: None,
            state: TaskState::Queued,
            last_sequence: 0,
            created_at: created_at.clone(),
            updated_at: created_at,
        }
    }

    /// Attach the secret-free effective-configuration manifest used at start.
    #[must_use]
    pub fn with_configuration_manifest(mut self, manifest_hash: impl Into<String>) -> Self {
        self.configuration_manifest_hash = Some(manifest_hash.into());
        self
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_projectless_conversation_omits_its_project_rather_than_nulling_it() {
        let record = ConversationRecord::new(
            ConversationId::new("c1"),
            None,
            "Untitled",
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let json = serde_json::to_value(&record).unwrap();
        assert!(!json.as_object().unwrap().contains_key("projectId"));
        assert!(!json.as_object().unwrap().contains_key("archivedAt"));
    }

    #[test]
    fn a_new_task_starts_queued_at_sequence_zero() {
        let record = TaskRecord::new(
            TaskId::new("t1"),
            None,
            None,
            "say hello",
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        assert_eq!(record.state, TaskState::Queued);
        assert_eq!(record.last_sequence, 0);
        assert_eq!(record.created_at, record.updated_at);
        assert!(record.configuration_manifest_hash.is_none());
    }

    #[test]
    fn records_serialize_as_camel_case() {
        let record = ProjectRecord::new(
            ProjectId::new("p1"),
            "Demo",
            vec!["/projects/demo".to_owned()],
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["projectId"], "p1");
        assert_eq!(json["createdAt"], "2026-01-01T00:00:00.000Z");
        assert_eq!(json["roots"][0], "/projects/demo");
    }
}

/// Where a search found what it was asked for (ADR 0059).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SearchField {
    /// The conversation's title.
    Title,
    /// What was asked in one of its messages.
    Goal,
    /// The name or folder of the project it belongs to.
    Project,
}

/// A conversation a search found, and why (ADR 0059).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHit {
    /// The conversation.
    pub conversation: ConversationRecord,
    /// The name of its project, when it belongs to one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_name: Option<String>,
    /// Where the match was.
    pub matched_in: SearchField,
    /// The message that matched, when it was a message.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub excerpt: Option<String>,
}

/// What a search of conversation titles and project metadata found
/// (`PHASE_1_SPEC.md` §4.1, ADR 0059).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    /// Contract schema version.
    pub schema_version: u32,
    /// Matching conversations, newest first.
    pub conversations: Vec<ConversationHit>,
    /// Matching projects, by name or folder.
    pub projects: Vec<ProjectRecord>,
}
