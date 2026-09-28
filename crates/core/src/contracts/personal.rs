//! What the person tells Tramontana about themselves (ADR 0061).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{SCHEMA_VERSION, Timestamp};

/// How answers should be written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AnswerStyle {
    /// Short answers, as fast as possible.
    Quick,
    /// Clear, complete answers for everyday work.
    #[default]
    Balanced,
    /// Takes longer, and thinks things through.
    Thorough,
}

/// One thing the person asked Tramontana to remember.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryNote {
    /// Stable identifier.
    pub note_id: String,
    /// The note, redacted when it was saved.
    pub text: String,
    /// When it was added.
    pub created_at: Timestamp,
}

/// The person's profile: kept in the local profile, never in a project, and
/// given only to chats (ADR 0061).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonalProfile {
    /// Contract schema version.
    pub schema_version: u32,
    /// What to call the person.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    /// A sentence or two about their work.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub about: Option<String>,
    /// How answers should be written.
    #[serde(default)]
    pub answer_style: AnswerStyle,
    /// Whether the notes are given to chats. Notes are kept either way.
    pub memory_enabled: bool,
    /// The notes, oldest first.
    #[serde(default)]
    pub notes: Vec<MemoryNote>,
}

impl Default for PersonalProfile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: None,
            about: None,
            answer_style: AnswerStyle::Balanced,
            memory_enabled: true,
            notes: Vec::new(),
        }
    }
}

/// The parts of the profile a person edits together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonalDetails {
    /// What to call the person; empty clears it.
    #[serde(default)]
    pub name: String,
    /// About their work; empty clears it.
    #[serde(default)]
    pub about: String,
    /// How answers should be written.
    pub answer_style: AnswerStyle,
    /// Whether the notes are given to chats.
    pub memory_enabled: bool,
}
