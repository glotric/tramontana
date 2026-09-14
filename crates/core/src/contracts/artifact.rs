//! What a task produced, recorded as Tramontana's own fact.
//!
//! `CORE_CONTRACTS.md` §5.1 lists `artifactRecorded`: "Artifact reference,
//! media/hash/size/data class, creator and relation." The first artifact is
//! the changed-file summary: which files a task created, edited, moved and
//! deleted, as the tools that did it reported. It is built from the task's own
//! `toolCompleted` events, so it covers a file Git does not track yet, and a
//! task that failed or was cancelled half-way (ADR 0026).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ArtifactId, DataClass, content_hash_bytes};

/// What one invocation did to one file, as the tool that did it reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    /// What happened.
    pub change: FileChangeKind,
    /// The project-relative path the file has now; for a deletion, the path
    /// it had.
    pub path: String,
    /// For a move, the project-relative path it had before.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub from: Option<String>,
    /// Hash of the content afterwards; for a deletion, of the content that
    /// was removed and kept for recovery.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content_hash: Option<String>,
    /// For an edit, the hash of the content before it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub previous_hash: Option<String>,
    /// Whether the tool found the change already made and changed nothing
    /// now: a replay after an interruption, or content that was already
    /// there.
    #[serde(default)]
    pub already_applied: bool,
}

impl FileChange {
    /// A few words for one change.
    #[must_use]
    pub fn describe(&self) -> String {
        let already = if self.already_applied {
            " (already so)"
        } else {
            ""
        };
        match (self.change, &self.from) {
            (FileChangeKind::Moved, Some(from)) => {
                format!("moved {from} to {}{already}", self.path)
            }
            (change, _) => format!("{} {}{already}", change.verb(), self.path),
        }
    }
}

/// The kinds of change a file tool makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum FileChangeKind {
    /// A new file.
    Created,
    /// An existing file's content changed.
    Modified,
    /// A file got a new name.
    Moved,
    /// A file was removed.
    Deleted,
}

impl FileChangeKind {
    fn verb(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Moved => "moved",
            Self::Deleted => "deleted",
        }
    }
}

/// An artifact recorded against a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRecord {
    /// Identifies it within the task's record.
    pub artifact_id: ArtifactId,
    /// A short, safe name for it.
    pub label: String,
    /// Who produced it.
    pub produced_by: ArtifactProducer,
    /// How it relates to the task.
    pub relation: ArtifactRelation,
    /// What its content is.
    pub media_type: String,
    /// How sensitive its content is.
    pub data_class: DataClass,
    /// SHA-256 of the content's JSON, so a copy can be checked.
    pub content_hash: String,
    /// Size of that JSON in bytes.
    pub byte_count: u64,
    /// The content itself, small enough to carry in the record.
    pub content: ArtifactContent,
}

/// Who produced an artifact.
///
/// Records distinguish Tramontana's own facts from claims a worker makes
/// (`PROJECT_FORMAT.md` §7). Only Tramontana produces artifacts so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ArtifactProducer {
    /// Derived by Tramontana from its own records.
    Tramontana,
}

/// How an artifact relates to its task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ArtifactRelation {
    /// A summary of what the task did.
    TaskSummary,
}

/// What an artifact holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum ArtifactContent {
    /// Every file change the task's tools reported, in the order they
    /// happened.
    #[serde(rename_all = "camelCase")]
    ChangedFiles {
        /// The changes.
        changes: Vec<FileChange>,
    },
}

/// The media type of a changed-file summary.
pub const CHANGED_FILES_MEDIA_TYPE: &str = "application/vnd.tramontana.changed-files+json";

/// How many changes a one-line summary names before counting the rest.
const NAMED_IN_SUMMARY: usize = 5;

impl ArtifactRecord {
    /// A changed-file summary of `changes`, hashed and measured.
    #[must_use]
    pub fn changed_files(artifact_id: ArtifactId, changes: Vec<FileChange>) -> Self {
        let content = ArtifactContent::ChangedFiles { changes };
        // These types serialize infallibly; an empty body would only make the
        // hash describe nothing, never claim content that is not there.
        let bytes = serde_json::to_vec(&content).unwrap_or_default();
        Self {
            artifact_id,
            label: "Files this task changed".to_owned(),
            produced_by: ArtifactProducer::Tramontana,
            relation: ArtifactRelation::TaskSummary,
            media_type: CHANGED_FILES_MEDIA_TYPE.to_owned(),
            data_class: DataClass::Project,
            content_hash: content_hash_bytes(&bytes),
            byte_count: bytes.len() as u64,
            content,
        }
    }

    /// One line a person can read, for activity views and handoffs.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.content {
            ArtifactContent::ChangedFiles { changes } => {
                let named: Vec<String> = changes
                    .iter()
                    .take(NAMED_IN_SUMMARY)
                    .map(FileChange::describe)
                    .collect();
                let rest = changes.len().saturating_sub(NAMED_IN_SUMMARY);
                let tail = if rest == 0 {
                    String::new()
                } else {
                    format!(", and {rest} more")
                };
                format!(
                    "{} file change(s): {}{tail}",
                    changes.len(),
                    named.join(", ")
                )
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn change(change: FileChangeKind, path: &str) -> FileChange {
        FileChange {
            change,
            path: path.to_owned(),
            from: None,
            content_hash: Some("h".to_owned()),
            previous_hash: None,
            already_applied: false,
        }
    }

    #[test]
    fn a_summary_names_each_change_and_counts_the_rest() {
        let mut moved = change(FileChangeKind::Moved, "docs/b.md");
        moved.from = Some("b.md".to_owned());
        let mut again = change(FileChangeKind::Modified, "README.md");
        again.already_applied = true;
        let record = ArtifactRecord::changed_files(
            ArtifactId::new("artifact-1"),
            vec![change(FileChangeKind::Created, "src/new.rs"), moved, again],
        );

        assert_eq!(
            record.summary(),
            "3 file change(s): created src/new.rs, moved b.md to docs/b.md, modified README.md \
             (already so)"
        );

        let many = ArtifactRecord::changed_files(
            ArtifactId::new("artifact-2"),
            (0..7)
                .map(|n| change(FileChangeKind::Deleted, &format!("f{n}")))
                .collect(),
        );
        assert!(
            many.summary().ends_with("deleted f4, and 2 more"),
            "{}",
            many.summary()
        );
    }

    #[test]
    fn the_hash_and_size_describe_exactly_the_content() {
        let record = ArtifactRecord::changed_files(
            ArtifactId::new("artifact-1"),
            vec![change(FileChangeKind::Created, "a.txt")],
        );
        let bytes = serde_json::to_vec(&record.content).unwrap();
        assert_eq!(record.content_hash, content_hash_bytes(&bytes));
        assert_eq!(record.byte_count, bytes.len() as u64);
        assert_eq!(record.data_class, DataClass::Project);
        assert_eq!(record.produced_by, ArtifactProducer::Tramontana);
    }
}
