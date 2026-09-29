//! Speech to text, on this device (ADR 0065).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::RuntimeProvenance;

/// The `whisper-cli` and model the person chose, as they were then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DictationSetup {
    /// The `whisper-cli` executable.
    pub binary_path: String,
    /// The model file.
    pub model_path: String,
    /// A language code such as `en`, or absent for `auto`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub language: Option<String>,
    /// The files' fingerprints when chosen (ADR 0043).
    pub provenance: RuntimeProvenance,
}

/// Whether dictation can be used, and with what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DictationStatus {
    /// Whether a transcriber has been chosen.
    pub configured: bool,
    /// The chosen executable.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub binary_path: Option<String>,
    /// The chosen model.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_path: Option<String>,
    /// The chosen language, absent for `auto`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub language: Option<String>,
}
