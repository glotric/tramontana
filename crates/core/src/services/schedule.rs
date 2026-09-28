//! When a schedule runs (ADR 0063).
//!
//! All in local wall-clock time, to the minute, with no time-zone rules: a
//! schedule is due when its most recent occurrence at or before now is later
//! than its last run. The same rule catches up after the application was
//! closed, once, and needs no special case for daylight-saving jumps.

use crate::contracts::{
    Cadence, CorrelationId, ErrorCode, ErrorEnvelope, LocalDateTime, NewSchedule, Schedule, Weekday,
};

/// The longest schedule title kept, in bytes.
pub const SCHEDULE_TITLE_BYTES: usize = 120;

/// The longest goal a schedule runs, in bytes.
pub const SCHEDULE_GOAL_BYTES: usize = 4_000;

/// At most this many schedules are kept.
pub const SCHEDULES: usize = 100;

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        u8::try_from(month).unwrap_or(1),
        u8::try_from(day).unwrap_or(1),
    )
}

/// The weekday of a civil date.
#[must_use]
pub fn weekday_of(date: &LocalDateTime) -> Weekday {
    // 1970-01-01 was a Thursday.
    match (days_from_civil(date.year, date.month, date.day) + 3).rem_euclid(7) {
        0 => Weekday::Monday,
        1 => Weekday::Tuesday,
        2 => Weekday::Wednesday,
        3 => Weekday::Thursday,
        4 => Weekday::Friday,
        5 => Weekday::Saturday,
        _ => Weekday::Sunday,
    }
}

fn shifted(date: &LocalDateTime, days: i64, hour: u8, minute: u8) -> LocalDateTime {
    let (year, month, day) =
        civil_from_days(days_from_civil(date.year, date.month, date.day) + days);
    LocalDateTime {
        year,
        month,
        day,
        hour,
        minute,
    }
}

fn time_of(cadence: Cadence) -> (u8, u8) {
    match cadence {
        Cadence::Daily { hour, minute }
        | Cadence::Weekdays { hour, minute }
        | Cadence::Weekly { hour, minute, .. } => (hour, minute),
    }
}

fn runs_on(cadence: Cadence, weekday: Weekday) -> bool {
    match cadence {
        Cadence::Daily { .. } => true,
        Cadence::Weekdays { .. } => weekday.index() < 5,
        Cadence::Weekly { weekday: day, .. } => day == weekday,
    }
}

/// The most recent occurrence at or before `now`.
#[must_use]
pub fn latest_occurrence(cadence: Cadence, now: &LocalDateTime) -> LocalDateTime {
    let (hour, minute) = time_of(cadence);
    for back in 0..=7 {
        let candidate = shifted(now, -back, hour, minute);
        if candidate <= *now && runs_on(cadence, weekday_of(&candidate)) {
            return candidate;
        }
    }
    // Every cadence occurs at least weekly, so eight days back always holds one.
    shifted(now, -8, hour, minute)
}

/// The first occurrence after `now`.
#[must_use]
pub fn next_occurrence(cadence: Cadence, now: &LocalDateTime) -> LocalDateTime {
    let (hour, minute) = time_of(cadence);
    for ahead in 0..=7 {
        let candidate = shifted(now, ahead, hour, minute);
        if candidate > *now && runs_on(cadence, weekday_of(&candidate)) {
            return candidate;
        }
    }
    shifted(now, 8, hour, minute)
}

/// The occurrence `schedule` is due for at `now`, if it is due.
#[must_use]
pub fn due_occurrence(schedule: &Schedule, now: &LocalDateTime) -> Option<LocalDateTime> {
    if !schedule.enabled {
        return None;
    }
    let latest = latest_occurrence(schedule.cadence, now);
    let after_last = schedule.last_run.is_none_or(|last| latest > last);
    (after_last && latest >= schedule.created_local).then_some(latest)
}

fn refused(message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::InvalidRequest,
        message,
        CorrelationId::new("schedule"),
    )
}

/// Check a new schedule, returning its title and redacted goal.
///
/// # Errors
///
/// [`ErrorCode::InvalidRequest`] for an empty or long title or goal, or a
/// time that is not on a clock.
pub fn check_new_schedule(new: &NewSchedule) -> Result<(String, String), ErrorEnvelope> {
    let title = new.title.trim();
    if title.is_empty() || title.len() > SCHEDULE_TITLE_BYTES || title.chars().any(char::is_control)
    {
        return Err(refused(format!(
            "A scheduled task needs a title of at most {SCHEDULE_TITLE_BYTES} bytes"
        )));
    }
    let goal = new.goal.trim();
    if goal.is_empty() || goal.len() > SCHEDULE_GOAL_BYTES {
        return Err(refused(format!(
            "A scheduled task needs something to do, in at most {SCHEDULE_GOAL_BYTES} bytes"
        )));
    }
    let (hour, minute) = time_of(new.cadence);
    if hour > 23 || minute > 59 {
        return Err(refused(
            "A scheduled task's time must be on a 24-hour clock",
        ));
    }
    Ok((super::redact(title).text, super::redact(goal).text))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;
    use crate::contracts::{ConversationId, RoutingMode, SCHEMA_VERSION, Timestamp};

    fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> LocalDateTime {
        LocalDateTime {
            year,
            month,
            day,
            hour,
            minute,
        }
    }

    fn schedule(cadence: Cadence, created: LocalDateTime) -> Schedule {
        Schedule {
            schema_version: SCHEMA_VERSION,
            schedule_id: "s1".into(),
            title: "Morning".into(),
            goal: "Plan my day.".into(),
            project_id: None,
            conversation_id: ConversationId::new("c1"),
            cadence,
            routing_mode: RoutingMode::LocalOnly,
            enabled: true,
            created_at: Timestamp::new("2026-09-28T00:00:00.000Z"),
            created_local: created,
            last_run: None,
            last_task_id: None,
        }
    }

    #[test]
    fn weekdays_and_dates_are_right_across_months_years_and_leap_days() {
        // 2026-09-28 is a Monday.
        assert_eq!(weekday_of(&at(2026, 9, 28, 0, 0)), Weekday::Monday);
        assert_eq!(weekday_of(&at(2026, 10, 4, 0, 0)), Weekday::Sunday);
        assert_eq!(weekday_of(&at(2028, 2, 29, 0, 0)), Weekday::Tuesday);
        assert_eq!(
            shifted(&at(2026, 12, 31, 0, 0), 1, 8, 0),
            at(2027, 1, 1, 8, 0)
        );
        assert_eq!(
            shifted(&at(2028, 3, 1, 0, 0), -1, 8, 0),
            at(2028, 2, 29, 8, 0)
        );
    }

    #[test]
    fn occurrences_follow_each_cadence() {
        let weekdays = Cadence::Weekdays { hour: 8, minute: 0 };
        // Saturday at noon: the last was Friday 8:00, the next Monday 8:00.
        let saturday = at(2026, 10, 3, 12, 0);
        assert_eq!(
            latest_occurrence(weekdays, &saturday),
            at(2026, 10, 2, 8, 0)
        );
        assert_eq!(next_occurrence(weekdays, &saturday), at(2026, 10, 5, 8, 0));

        let daily = Cadence::Daily {
            hour: 7,
            minute: 30,
        };
        assert_eq!(
            latest_occurrence(daily, &at(2026, 9, 28, 7, 30)),
            at(2026, 9, 28, 7, 30)
        );
        assert_eq!(
            latest_occurrence(daily, &at(2026, 9, 28, 7, 29)),
            at(2026, 9, 27, 7, 30)
        );
        assert_eq!(
            next_occurrence(daily, &at(2026, 9, 28, 7, 30)),
            at(2026, 9, 29, 7, 30)
        );

        let friday = Cadence::Weekly {
            weekday: Weekday::Friday,
            hour: 15,
            minute: 0,
        };
        assert_eq!(
            latest_occurrence(friday, &at(2026, 10, 2, 14, 59)),
            at(2026, 9, 25, 15, 0)
        );
        assert_eq!(
            next_occurrence(friday, &at(2026, 10, 2, 15, 0)),
            at(2026, 10, 9, 15, 0)
        );
    }

    #[test]
    fn a_schedule_is_due_once_per_occurrence_and_never_before_it_was_made() {
        let cadence = Cadence::Daily { hour: 8, minute: 0 };
        // Made at 9:00: today's 8:00 had passed, so nothing is due until tomorrow.
        let mut made = schedule(cadence, at(2026, 9, 28, 9, 0));
        assert_eq!(due_occurrence(&made, &at(2026, 9, 28, 9, 1)), None);
        assert_eq!(
            due_occurrence(&made, &at(2026, 9, 29, 8, 0)),
            Some(at(2026, 9, 29, 8, 0))
        );
        made.last_run = Some(at(2026, 9, 29, 8, 0));
        assert_eq!(due_occurrence(&made, &at(2026, 9, 29, 12, 0)), None);

        made.enabled = false;
        assert_eq!(due_occurrence(&made, &at(2026, 9, 30, 8, 0)), None);
    }

    #[test]
    fn a_week_away_catches_up_once_with_the_latest_occurrence() {
        let mut made = schedule(
            Cadence::Weekdays { hour: 8, minute: 0 },
            at(2026, 9, 1, 0, 0),
        );
        made.last_run = Some(at(2026, 9, 18, 8, 0));
        // Back on Wednesday afternoon, a week and a half later.
        assert_eq!(
            due_occurrence(&made, &at(2026, 9, 30, 14, 0)),
            Some(at(2026, 9, 30, 8, 0))
        );
    }

    #[test]
    fn a_repeated_hour_in_autumn_does_not_run_twice() {
        let mut made = schedule(
            Cadence::Daily {
                hour: 2,
                minute: 30,
            },
            at(2026, 10, 1, 0, 0),
        );
        made.last_run = Some(at(2026, 10, 25, 2, 30));
        // The clock goes back and reads 2:45 again: already run.
        assert_eq!(due_occurrence(&made, &at(2026, 10, 25, 2, 45)), None);
    }

    #[test]
    fn a_new_schedule_is_checked_and_its_goal_redacted() {
        let key = format!("sk-proj-{}", "a".repeat(40));
        let new = NewSchedule {
            title: " Morning ".into(),
            goal: format!("Use {key} to plan."),
            project_id: None,
            cadence: Cadence::Daily { hour: 8, minute: 0 },
            routing_mode: RoutingMode::LocalOnly,
        };
        let (title, goal) = check_new_schedule(&new).unwrap();
        assert_eq!(title, "Morning");
        assert!(!goal.contains(&key));
        let late = NewSchedule {
            cadence: Cadence::Daily {
                hour: 24,
                minute: 0,
            },
            ..new.clone()
        };
        assert!(check_new_schedule(&late).is_err());
        let empty = NewSchedule {
            goal: " ".into(),
            ..new
        };
        assert!(check_new_schedule(&empty).is_err());
    }
}
