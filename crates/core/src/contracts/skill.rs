//! Skills the person keeps for themselves (ADR 0070).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A skill in the person's own library, as a list shows it: what it is
/// called and what it is for, without its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonalSkill {
    /// Contract schema version.
    pub schema_version: u32,
    /// The name `/` finds it by: lowercase letters, digits and hyphens.
    pub id: String,
    /// Its declared name, or its id.
    pub name: String,
    /// What it is for, in a line, when it says.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    /// The content hash of its redacted text.
    pub content_hash: String,
    /// The length of its redacted text in bytes.
    pub byte_count: u64,
    /// What was removed from it as a credential, when anything was.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub redactions: Vec<String>,
}

/// A personal skill a task was given, as it was when the task started: its
/// redacted text is the guidance the model read, and a resume gives the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskSkill {
    /// The skill's id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its redacted text.
    pub text: String,
    /// The content hash of `text`.
    pub content_hash: String,
    /// What was removed from it as a credential.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub redactions: Vec<String>,
}
