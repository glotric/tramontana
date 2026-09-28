//! Regular work Tramontana does while it is open (ADR 0063).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ConversationId, ProjectId, RoutingMode, TaskId, Timestamp};

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Weekday {
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday.
    Sunday,
}

impl Weekday {
    /// Monday is 0, Sunday is 6.
    #[must_use]
    pub fn index(self) -> u8 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }
}

/// A local wall-clock date and time, to the minute: what a person means by
/// "Friday at 15:00" on this computer.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct LocalDateTime {
    /// The year.
    pub year: i32,
    /// The month, 1 to 12.
    pub month: u8,
    /// The day of the month, 1 to 31.
    pub day: u8,
    /// The hour, 0 to 23.
    pub hour: u8,
    /// The minute, 0 to 59.
    pub minute: u8,
}

/// When a schedule runs, in local wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Cadence {
    /// Every day at a time.
    #[serde(rename_all = "camelCase")]
    Daily {
        /// The hour, 0 to 23.
        hour: u8,
        /// The minute, 0 to 59.
        minute: u8,
    },
    /// Monday to Friday at a time.
    #[serde(rename_all = "camelCase")]
    Weekdays {
        /// The hour, 0 to 23.
        hour: u8,
        /// The minute, 0 to 59.
        minute: u8,
    },
    /// One day a week at a time.
    #[serde(rename_all = "camelCase")]
    Weekly {
        /// The day.
        weekday: Weekday,
        /// The hour, 0 to 23.
        hour: u8,
        /// The minute, 0 to 59.
        minute: u8,
    },
}

/// Regular work: a goal, where it runs, and when (ADR 0063).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable identifier.
    pub schedule_id: String,
    /// What the person calls it.
    pub title: String,
    /// What each run is asked to do, redacted when saved.
    pub goal: String,
    /// The project it works in; absent for a chat.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<ProjectId>,
    /// The conversation every run is a turn in.
    pub conversation_id: ConversationId,
    /// When it runs.
    pub cadence: Cadence,
    /// Where its models may run.
    pub routing_mode: RoutingMode,
    /// Whether it runs at all.
    pub enabled: bool,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it was created, in local wall-clock time: no occurrence before
    /// this runs.
    pub created_local: LocalDateTime,
    /// The occurrence it last ran for, or passed because a run was still
    /// under way.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_run: Option<LocalDateTime>,
    /// The task its last run started.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_task_id: Option<TaskId>,
}

/// A schedule, and when it runs next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleView {
    /// The schedule.
    pub schedule: Schedule,
    /// Its next occurrence after now, in local time; absent when it is off.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub next_run: Option<LocalDateTime>,
}

/// What a person fills in to create a schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewSchedule {
    /// What to call it.
    pub title: String,
    /// What each run is asked to do.
    pub goal: String,
    /// The project to work in; absent for a chat.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<ProjectId>,
    /// When it runs.
    pub cadence: Cadence,
    /// Where its models may run.
    pub routing_mode: RoutingMode,
}
