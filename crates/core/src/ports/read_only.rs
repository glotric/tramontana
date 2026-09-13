//! Bounded, canonical read-only turns available to any brokered worker.
use serde::{Deserialize, Serialize};

/// Hard limits for a reference session, charged before dispatch and retained
/// across restart. Reservations are conservative, not actual usage/cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadOnlyLimits {
    /// Maximum model/recovery turns.
    pub steps: u64,
    /// Maximum tool invocations (one slot is reserved per turn).
    pub tools: u64,
    /// Total reserved active time, excluding time while stopped.
    pub time_ms: u64,
    /// Cumulative serialized model input.
    pub input_bytes: u64,
    /// Cumulative model/tool output reservations.
    pub output_bytes: u64,
    /// Cumulative model output token reservations.
    pub output_tokens: u64,
    /// Deadline for one complete turn, including approval and tools.
    pub turn_ms: u64,
    /// Maximum model content bytes per turn.
    pub turn_bytes: usize,
    /// Maximum model output tokens per turn.
    pub turn_tokens: u32,
}

impl Default for ReadOnlyLimits {
    fn default() -> Self {
        Self {
            steps: 12,
            tools: 12,
            time_ms: 600_000,
            input_bytes: 8_388_608,
            output_bytes: 4_194_304,
            output_tokens: 12_288,
            turn_ms: 30_000,
            turn_bytes: 65_536,
            turn_tokens: 1024,
        }
    }
}

/// A host-owned turn either produces observations or a final answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnlyTurn {
    /// Canonical observations were recorded; the worker may ask for another turn.
    Observed,
    /// The model supplied a finished, redacted answer.
    Completed {
        /// Safe final answer.
        summary: String,
    },
}
