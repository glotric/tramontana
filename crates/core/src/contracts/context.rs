//! What was assembled for a destination, and what was left out.
//!
//! `CORE_CONTRACTS.md` §10 is normative. A context manifest is the record of a
//! *data-release decision*: it says what was gathered, what was actually sent,
//! and — the part that matters — what was withheld and why.
//!
//! Two properties make it worth having rather than decorative:
//!
//! - **It names sources by hash, not by content.** The manifest explains what
//!   was released without becoming a second copy of it, so it can be shown,
//!   stored, and exported when the content itself cannot.
//! - **An exclusion is a recorded fact.** A source that was gathered and then
//!   dropped appears with `included: false` and a reason. A manifest that
//!   listed only what was sent could not distinguish "nothing was excluded"
//!   from "exclusions were not written down".
//!
//! M4 records this shape and fills it from fakes; M5 assembles real context
//! behind the same type (`BUILD_ORDER.md` §12).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DataClass, SCHEMA_VERSION, TaskId, Timestamp, configuration_hex};

/// Where a piece of context came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ContextCategory {
    /// The conversation, including the user's goal.
    Conversation,
    /// A file the user chose.
    SelectedFile,
    /// Project-authored instructions.
    Instruction,
    /// A named skill definition.
    Skill,
    /// Facts about the repository, such as its branch.
    RepositoryMetadata,
    /// Output an earlier tool produced.
    ToolResult,
}

/// Where assembled context is about to go.
///
/// The distinction is a policy one, not a technical one: crossing to `Cloud`
/// is a data-release decision that needs a disclosure, and `Worker` means a
/// process that is not the model and not Tramontana
/// (`SECURITY_AND_PERMISSIONS.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DestinationClass {
    /// A model running on this device.
    Local,
    /// A third-party endpoint.
    Cloud,
    /// A worker process.
    Worker,
}

/// One thing that was considered for a context bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextSource {
    /// Stable identifier for this source within the task.
    pub source_id: String,
    /// What kind of source it is.
    pub category: ContextCategory,
    /// How sensitive its content is.
    pub data_class: DataClass,
    /// Hash of the content, so the manifest explains without copying.
    pub content_hash: String,
    /// Size of the content in bytes.
    pub byte_count: u64,
    /// Approximate token cost, when one was estimated.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub token_estimate: Option<u64>,
    /// Whether it was actually sent.
    pub included: bool,
    /// Why it was not, when it was not.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exclusion_reason: Option<String>,
    /// Stable codes for credentials removed from this source at ingestion.
    ///
    /// Per source rather than only on the manifest, so a release plan can
    /// report the redactions that apply to what it actually carried instead
    /// of every redaction the task ever made.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub redactions: Vec<String>,
}

impl ContextSource {
    /// A source that was gathered and sent.
    #[must_use]
    pub fn included(
        source_id: impl Into<String>,
        category: ContextCategory,
        data_class: DataClass,
        content: &str,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            category,
            data_class,
            content_hash: content_hash(content),
            byte_count: content.len() as u64,
            token_estimate: None,
            included: true,
            exclusion_reason: None,
            redactions: Vec::new(),
        }
    }

    /// A source that was gathered, redacted, and sent.
    ///
    /// `content` must already be the redacted text: the hash names what was
    /// released, and hashing the original would make the record describe
    /// content that never left.
    #[must_use]
    pub fn redacted(
        source_id: impl Into<String>,
        category: ContextCategory,
        data_class: DataClass,
        content: &str,
        redactions: Vec<String>,
    ) -> Self {
        Self {
            redactions,
            ..Self::included(source_id, category, data_class, content)
        }
    }

    /// A source that was gathered and withheld, with a stable reason.
    #[must_use]
    pub fn excluded(
        source_id: impl Into<String>,
        category: ContextCategory,
        data_class: DataClass,
        content: &str,
        reason_code: impl Into<String>,
    ) -> Self {
        Self {
            exclusion_reason: Some(reason_code.into()),
            included: false,
            ..Self::included(source_id, category, data_class, content)
        }
    }
}

/// Hash of a source's content.
///
/// The same function the configuration manifest uses, so two records naming
/// one file agree about what it was.
#[must_use]
pub fn content_hash(content: &str) -> String {
    content_hash_bytes(content.as_bytes())
}

/// Hash of raw bytes, by the same function as [`content_hash`].
///
/// For content that is not guaranteed to be text — a file on disk that a
/// write tool must verify before replacing it — so the hash a read reports and
/// the hash a patch checks are computed the same way, from the same bytes.
#[must_use]
pub fn content_hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    configuration_hex(&Sha256::digest(bytes))
}

/// Everything assembled for one destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextManifest {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier, referenced by routing decisions and checkpoints.
    pub context_manifest_id: String,
    /// The task this was assembled for.
    pub task_id: TaskId,
    /// Where it was going.
    pub destination_class: DestinationClass,
    /// Everything considered, included or not.
    pub sources: Vec<ContextSource>,
    /// Total bytes actually sent.
    pub total_bytes: u64,
    /// Approximate total token cost, when estimated.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub total_token_estimate: Option<u64>,
    /// Stable codes for redactions applied before release.
    pub redactions: Vec<String>,
    /// When it was assembled.
    pub created_at: Timestamp,
}

impl ContextManifest {
    /// Build a manifest from the sources that were considered.
    ///
    /// `total_bytes` counts only what was *included*: it answers "how much was
    /// released", and counting withheld sources in it would make the number
    /// mean the opposite of what a reader assumes.
    #[must_use]
    pub fn new(
        context_manifest_id: impl Into<String>,
        task_id: TaskId,
        destination_class: DestinationClass,
        sources: Vec<ContextSource>,
        created_at: Timestamp,
    ) -> Self {
        let total_bytes = sources
            .iter()
            .filter(|source| source.included)
            .map(|source| source.byte_count)
            .sum();
        // Aggregated from the sources rather than passed in, for the same
        // reason `total_bytes` is: a manifest whose summary could disagree
        // with its own contents is a record of nothing.
        let mut redactions: Vec<String> = sources
            .iter()
            .flat_map(|source| source.redactions.iter().cloned())
            .collect();
        redactions.sort_unstable();
        redactions.dedup();

        Self {
            schema_version: SCHEMA_VERSION,
            context_manifest_id: context_manifest_id.into(),
            task_id,
            destination_class,
            sources,
            total_bytes,
            total_token_estimate: None,
            redactions,
            created_at,
        }
    }

    /// Whether any source carries data the user marked sensitive.
    ///
    /// Only counts sources that were actually sent: a sensitive source that
    /// was withheld is the system working, not a disclosure.
    #[must_use]
    pub fn releases_sensitive_data(&self) -> bool {
        self.sources
            .iter()
            .any(|source| source.included && source.data_class == DataClass::Sensitive)
    }

    /// The data classes actually released, strongest first.
    ///
    /// What a routing decision turns on: sending public content to a cloud
    /// endpoint is not the same act as sending a user's project to one.
    #[must_use]
    pub fn released_data_classes(&self) -> Vec<DataClass> {
        let mut classes: Vec<DataClass> = self
            .sources
            .iter()
            .filter(|source| source.included)
            .map(|source| source.data_class)
            .collect();
        classes.sort_unstable_by_key(|class| match class {
            DataClass::Secret => 0,
            DataClass::Sensitive => 1,
            DataClass::Project => 2,
            DataClass::Public => 3,
        });
        classes.dedup();
        classes
    }

    /// Sources that were gathered and withheld.
    pub fn exclusions(&self) -> impl Iterator<Item = &ContextSource> {
        self.sources.iter().filter(|source| !source.included)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn manifest(sources: Vec<ContextSource>) -> ContextManifest {
        ContextManifest::new(
            "context-1",
            TaskId::new("task-1"),
            DestinationClass::Local,
            sources,
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        )
    }

    #[test]
    fn total_bytes_counts_what_was_released_not_what_was_gathered() {
        let manifest = manifest(vec![
            ContextSource::included(
                "goal",
                ContextCategory::Conversation,
                DataClass::Project,
                "hello",
            ),
            ContextSource::excluded(
                "secrets.env",
                ContextCategory::SelectedFile,
                DataClass::Sensitive,
                "a very long secret file",
                "context.sensitivityBlocked",
            ),
        ]);
        assert_eq!(manifest.total_bytes, 5, "a withheld source was counted");
        assert_eq!(manifest.exclusions().count(), 1);
    }

    #[test]
    fn a_withheld_sensitive_source_is_not_a_disclosure() {
        // The distinction the type exists for: gathering something sensitive
        // and refusing to send it is the system working.
        let withheld = manifest(vec![ContextSource::excluded(
            "notes",
            ContextCategory::SelectedFile,
            DataClass::Sensitive,
            "private",
            "context.sensitivityBlocked",
        )]);
        assert!(!withheld.releases_sensitive_data());

        let sent = manifest(vec![ContextSource::included(
            "notes",
            ContextCategory::SelectedFile,
            DataClass::Sensitive,
            "private",
        )]);
        assert!(sent.releases_sensitive_data());
    }

    #[test]
    fn an_exclusion_always_carries_a_reason() {
        let source = ContextSource::excluded(
            "big.log",
            ContextCategory::ToolResult,
            DataClass::Project,
            "…",
            "context.tooLarge",
        );
        assert!(!source.included);
        assert_eq!(source.exclusion_reason.as_deref(), Some("context.tooLarge"));
    }

    #[test]
    fn a_source_is_named_by_hash_rather_than_content() {
        let source = ContextSource::included(
            "goal",
            ContextCategory::Conversation,
            DataClass::Project,
            "a private goal",
        );
        let json = serde_json::to_string(&source).unwrap();
        assert!(
            !json.contains("a private goal"),
            "the manifest copied the content it was supposed to describe"
        );
        assert_eq!(source.content_hash.len(), 64);
    }

    #[test]
    fn manifests_serialize_as_camel_case_and_omit_absent_fields() {
        let json = serde_json::to_value(manifest(vec![ContextSource::included(
            "goal",
            ContextCategory::Conversation,
            DataClass::Project,
            "hello",
        )]))
        .unwrap();

        assert_eq!(json["contextManifestId"], "context-1");
        assert_eq!(json["destinationClass"], "local");
        assert_eq!(json["sources"][0]["category"], "conversation");
        assert_eq!(json["sources"][0]["included"], true);
        assert!(!json.as_object().unwrap().contains_key("totalTokenEstimate"));
        assert!(
            !json["sources"][0]
                .as_object()
                .unwrap()
                .contains_key("exclusionReason")
        );
    }

    #[test]
    fn identical_content_hashes_identically_across_records() {
        // A context manifest and a configuration manifest naming one file must
        // agree about what it was.
        assert_eq!(content_hash("same"), content_hash("same"));
        assert_ne!(content_hash("same"), content_hash("different"));
    }

    #[test]
    fn text_and_byte_hashes_agree() {
        // A read reports one and a patch checks the other; if they ever
        // diverged, every patch would be refused as a stale reading.
        assert_eq!(
            content_hash("héllo"),
            content_hash_bytes("héllo".as_bytes())
        );
    }
}
