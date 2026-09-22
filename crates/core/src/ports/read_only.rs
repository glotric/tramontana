//! Bounded, canonical model/observation turns available to any brokered worker.
use serde::{Deserialize, Serialize};

/// Which tools a turn may offer a model.
///
/// Read-only unless a composition root asks for more, and each switch is
/// asked for on its own. Admission only makes a tool offerable: every write
/// or recipe the model then asks for is still its own permission decision.
/// Beyond configured recipes, nothing dangerous, irreversible or external is
/// offered at any level (ADRs 0021 and 0025).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolAdmission {
    /// Writes whose effect is local and reversible.
    #[serde(default)]
    pub writes: bool,
    /// The project's configured recipes: the one dangerous tool a model may
    /// be offered, named by id and approved on every run (ADR 0025).
    #[serde(default)]
    pub recipes: bool,
    /// The tools of the profile's Apps, and only those an App says only read
    /// (ADR 0036). Every call is still asked about: what an App says about
    /// its own tool is not a permission (ADR 0035).
    #[serde(default)]
    pub apps: bool,
    /// The built-in browser (ADR 0047): opening pages and acting on them in a
    /// window of the task's own. The first visit to each site is asked
    /// about, and submitting a form every time.
    #[serde(default)]
    pub browse: bool,
}

impl ToolAdmission {
    /// Reads with no side effect: the M6 catalogue.
    pub const READ_ONLY: Self = Self {
        writes: false,
        recipes: false,
        apps: false,
        browse: false,
    };

    /// Reads, plus writes whose effect is local and reversible.
    pub const READ_WRITE: Self = Self {
        writes: true,
        recipes: false,
        apps: false,
        browse: false,
    };

    /// This level, with the profile's Apps admitted as well.
    #[must_use]
    pub const fn with_apps(self) -> Self {
        Self { apps: true, ..self }
    }

    /// This level, with the built-in browser admitted as well.
    #[must_use]
    pub const fn with_browse(self) -> Self {
        Self {
            browse: true,
            ..self
        }
    }

    /// This level, with the project's recipes admitted as well.
    #[must_use]
    pub const fn with_recipes(self) -> Self {
        Self {
            recipes: true,
            ..self
        }
    }

    /// Whether nothing beyond reads is admitted.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        !self.writes && !self.recipes && !self.browse
    }
}

/// Hard limits for a reference session, charged before dispatch and retained
/// across restart. Reservations are conservative, not actual usage/cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadOnlyLimits {
    /// Maximum model/recovery turns.
    pub steps: u64,
    /// Maximum tool invocations (one slot is reserved per turn).
    pub tools: u64,
    /// Total reserved model time, excluding time while stopped: each turn
    /// reserves `turn_ms`. Approval waits and tool runs are not charged to
    /// it; the step and tool limits bound how many runs a task makes
    /// (ADR 0027).
    pub time_ms: u64,
    /// Cumulative serialized model input.
    pub input_bytes: u64,
    /// Cumulative model/tool output reservations.
    pub output_bytes: u64,
    /// Cumulative model output token reservations.
    pub output_tokens: u64,
    /// Deadline for one model call. Waiting for approval is not timed, and a
    /// tool run is bounded by the tool and [`TOOL_RUN_LIMIT`] (ADR 0027).
    pub turn_ms: u64,
    /// Maximum model content bytes per turn.
    pub turn_bytes: usize,
    /// Maximum model output tokens per turn.
    pub turn_tokens: u32,
}

impl ReadOnlyLimits {
    /// The limits every task had until ADR 0047: twelve steps in ten
    /// minutes. A task recorded under them keeps them when it resumes.
    #[must_use]
    pub const fn before_adr_0047() -> Self {
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

/// Forty steps in thirty minutes (ADR 0047). Every other total is scaled so
/// that the step limit, not an incidental one, is what a long task meets:
/// each turn reserves 1 MiB of input, 320 KiB of output, 2,048 tokens and
/// 45 seconds, and forty of each fit.
impl Default for ReadOnlyLimits {
    fn default() -> Self {
        Self {
            steps: 40,
            tools: 40,
            time_ms: 1_800_000,
            input_bytes: 41_943_040,
            output_bytes: 16_777_216,
            output_tokens: 81_920,
            turn_ms: 45_000,
            turn_bytes: 65_536,
            turn_tokens: 2048,
        }
    }
}

/// The longest the broker lets one tool run.
///
/// A tool is expected to bound itself: the built-in file and Git tools stop
/// at 15 seconds, a recipe at ten minutes. This only stops one that does not,
/// so it is longer than any of them. It is not the turn's deadline, which
/// bounds the model call, and waiting for a person to approve the run is not
/// counted (ADR 0027).
pub const TOOL_RUN_LIMIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

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
