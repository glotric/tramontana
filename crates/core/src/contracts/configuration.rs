//! The effective-configuration record.
//!
//! `CONFIGURATION.md` §11 requires the interface to show every loaded source,
//! its scope, its precedence, its validation state, and a content hash. §12
//! adds that a task records a secret-free manifest of the configuration it
//! started with, so behavior can be explained later even after the files
//! change.
//!
//! Nothing here holds file *content*. The file is the source of truth; a hash
//! is enough to explain behavior without copying project data into the
//! database or an export.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{SCHEMA_VERSION, Timestamp};

/// What kind of instruction file a source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ConfigurationKind {
    /// `AGENTS.md` at the project root.
    RootAgents,
    /// `AGENTS.md` in a subdirectory, applying to that subtree.
    ScopedAgents,
    /// `CLAUDE.md`, loaded as lower-precedence compatibility content.
    Claude,
    /// `.tramontana/project.md`.
    ProjectOverview,
    /// A file under `.tramontana/rules/`.
    Rule,
    /// A file under `.tramontana/skills/`.
    Skill,
    /// A file under `.tramontana/agents/`.
    AgentProfile,
    /// A file under `.tramontana/workflows/`.
    Workflow,
}

impl ConfigurationKind {
    /// Where this kind sits in the precedence order of `CONFIGURATION.md` §4.
    ///
    /// Lower numbers win. The user's request and an explicitly selected
    /// workflow occupy ranks 1 and 2 and are not files, so file-backed sources
    /// start at 3.
    #[must_use]
    pub fn precedence(self) -> u32 {
        match self {
            Self::ScopedAgents => 3,
            Self::Rule => 4,
            Self::RootAgents => 5,
            Self::ProjectOverview => 6,
            Self::Claude => 7,
            Self::Skill | Self::AgentProfile | Self::Workflow => 8,
        }
    }
}

/// Why a source was rejected or flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DiagnosticSeverity {
    /// Loaded, but something is worth knowing.
    Warning,
    /// Not loaded. The item is disabled, and startup continues.
    Error,
}

/// Something noticed while loading a source.
///
/// `CONFIGURATION.md` §5: an invalid or duplicate definition disables the
/// affected item and shows a validation error. It never crashes startup, and a
/// broken file in one project must not make the application unusable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationDiagnostic {
    /// How serious it is.
    pub severity: DiagnosticSeverity,
    /// Project-relative path of the file concerned.
    pub relative_path: String,
    /// Human-readable explanation.
    pub message: String,
}

/// One loaded instruction file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationSource {
    /// Stable identifier for this source within the project.
    pub source_id: String,
    /// Project-relative path. Never absolute: an absolute path names a machine.
    pub relative_path: String,
    /// What kind of file it is.
    pub kind: ConfigurationKind,
    /// Declared `id` from front matter, when present.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub declared_id: Option<String>,
    /// Declared `name` from front matter, when present.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub declared_name: Option<String>,
    /// Whether the item is active. Front matter may set `enabled: false`, and
    /// a validation error also disables it.
    pub enabled: bool,
    /// Precedence rank; lower wins.
    pub precedence: u32,
    /// SHA-256 of the file's bytes, for reproducibility.
    pub content_hash: String,
    /// Size in bytes.
    pub byte_count: u64,
    /// Front matter keys this build does not recognize.
    ///
    /// Preserved rather than dropped, so a file authored for a newer version
    /// survives a round trip through an older one (`CONFIGURATION.md` §5).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub unknown_keys: Vec<String>,
}

/// Everything loaded for a project, in precedence order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveConfiguration {
    /// Contract schema version.
    pub schema_version: u32,
    /// Sources, strongest precedence first.
    pub sources: Vec<ConfigurationSource>,
    /// Problems found while loading.
    pub diagnostics: Vec<ConfigurationDiagnostic>,
    /// Hash over the ordered source hashes.
    ///
    /// One value that changes whenever the effective configuration changes, so
    /// a task can record what it started with in a single field
    /// (`CONFIGURATION.md` §11).
    pub manifest_hash: String,
    /// When this was resolved.
    pub resolved_at: Timestamp,
}

impl EffectiveConfiguration {
    /// Build a manifest from already-ordered sources.
    #[must_use]
    pub fn new(
        sources: Vec<ConfigurationSource>,
        diagnostics: Vec<ConfigurationDiagnostic>,
        resolved_at: Timestamp,
    ) -> Self {
        let manifest_hash = manifest_hash(&sources);
        Self {
            schema_version: SCHEMA_VERSION,
            sources,
            diagnostics,
            manifest_hash,
            resolved_at,
        }
    }

    /// Whether any source failed to load.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error)
    }
}

/// Hash the ordered source list.
///
/// Order is part of the input: the same files at different precedence produce
/// different behavior, so they must produce a different manifest hash.
fn manifest_hash(sources: &[ConfigurationSource]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for source in sources {
        hasher.update(source.relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(source.content_hash.as_bytes());
        hasher.update([0]);
        hasher.update(source.enabled.to_string().as_bytes());
        hasher.update([0]);
    }
    hex(&hasher.finalize())
}

/// Lowercase hex encoding.
///
/// Written out because `sha2` 0.11 returns a byte array that does not
/// implement `LowerHex`, and a hash that renders differently across versions
/// would change every manifest for no reason.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    fn source(path: &str, kind: ConfigurationKind, hash: &str) -> ConfigurationSource {
        ConfigurationSource {
            source_id: path.to_owned(),
            relative_path: path.to_owned(),
            kind,
            declared_id: None,
            declared_name: None,
            enabled: true,
            precedence: kind.precedence(),
            content_hash: hash.to_owned(),
            byte_count: 0,
            unknown_keys: Vec::new(),
        }
    }

    #[test]
    fn precedence_matches_the_documented_order() {
        // CONFIGURATION.md §4, strongest first among file-backed sources.
        use ConfigurationKind::{
            AgentProfile, Claude, ProjectOverview, RootAgents, Rule, ScopedAgents, Skill, Workflow,
        };
        assert!(ScopedAgents.precedence() < Rule.precedence());
        assert!(Rule.precedence() < RootAgents.precedence());
        assert!(RootAgents.precedence() < ProjectOverview.precedence());
        assert!(ProjectOverview.precedence() < Claude.precedence());
        assert!(Claude.precedence() < Skill.precedence());
        // Skills, agent profiles, and workflows share the weakest rank.
        assert_eq!(Skill.precedence(), AgentProfile.precedence());
        assert_eq!(Skill.precedence(), Workflow.precedence());
    }

    #[test]
    fn claude_md_never_outranks_agents_md() {
        // CLAUDE.md is compatibility content, explicitly weaker than the
        // project's own instructions.
        assert!(
            ConfigurationKind::Claude.precedence() > ConfigurationKind::RootAgents.precedence()
        );
    }

    #[test]
    fn the_manifest_hash_changes_when_content_changes() {
        let a = EffectiveConfiguration::new(
            vec![source("AGENTS.md", ConfigurationKind::RootAgents, "aaa")],
            Vec::new(),
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let b = EffectiveConfiguration::new(
            vec![source("AGENTS.md", ConfigurationKind::RootAgents, "bbb")],
            Vec::new(),
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        assert_ne!(a.manifest_hash, b.manifest_hash);
    }

    #[test]
    fn the_manifest_hash_changes_when_order_changes() {
        // The same files at different precedence produce different behavior,
        // so they must not hash the same.
        let first = source("a/AGENTS.md", ConfigurationKind::ScopedAgents, "aaa");
        let second = source("AGENTS.md", ConfigurationKind::RootAgents, "bbb");

        let forward = EffectiveConfiguration::new(
            vec![first.clone(), second.clone()],
            Vec::new(),
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        let reversed = EffectiveConfiguration::new(
            vec![second, first],
            Vec::new(),
            Timestamp::new("2026-01-01T00:00:00.000Z"),
        );
        assert_ne!(forward.manifest_hash, reversed.manifest_hash);
    }

    #[test]
    fn the_manifest_hash_changes_when_a_source_is_disabled() {
        let enabled = source("AGENTS.md", ConfigurationKind::RootAgents, "aaa");
        let mut disabled = enabled.clone();
        disabled.enabled = false;

        assert_ne!(
            EffectiveConfiguration::new(vec![enabled], Vec::new(), Timestamp::new("t"))
                .manifest_hash,
            EffectiveConfiguration::new(vec![disabled], Vec::new(), Timestamp::new("t"))
                .manifest_hash
        );
    }
}
