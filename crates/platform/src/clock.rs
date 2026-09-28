use std::time::{SystemTime, UNIX_EPOCH};

use tramontana_core::contracts::Timestamp;
use tramontana_core::ports::Clock;

/// The system clock, formatted as UTC RFC 3339.
///
/// Formatted by hand rather than with a date library: the domain never does
/// arithmetic on a timestamp, it only orders and displays them, and a fixed-
/// width UTC representation sorts correctly as text. That keeps a dependency
/// out of the one place every record touches.
#[derive(Debug, Default)]
pub struct SystemClock;

impl SystemClock {
    /// A clock reading the operating system time.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Timestamp::new(format_rfc3339(elapsed.as_secs(), elapsed.subsec_millis()))
    }
}

/// The computer's local wall-clock time, read from the operating system's
/// time zone each time it is asked (ADR 0063), so a changed zone is
/// followed at once.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemLocalClock;

impl SystemLocalClock {
    /// A clock reading the operating system's local time.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl tramontana_core::ports::LocalClock for SystemLocalClock {
    fn local_now(&self) -> tramontana_core::contracts::LocalDateTime {
        use chrono::{Datelike, Timelike};
        let now = chrono::Local::now();
        let small = |value: u32| u8::try_from(value).unwrap_or(0);
        tramontana_core::contracts::LocalDateTime {
            year: now.year(),
            month: small(now.month()),
            day: small(now.day()),
            hour: small(now.hour()),
            minute: small(now.minute()),
        }
    }
}

/// Format seconds since the Unix epoch as `YYYY-MM-DDThh:mm:ss.sssZ`.
fn format_rfc3339(seconds: u64, millis: u32) -> String {
    let days = seconds / 86_400;
    let time_of_day = seconds % 86_400;
    let (hours, minutes, secs) = (
        time_of_day / 3_600,
        (time_of_day / 60) % 60,
        time_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{secs:02}.{millis:03}Z")
}

/// Convert days since 1970-01-01 into a civil calendar date.
///
/// Howard Hinnant's `civil_from_days`, which is exact for the proleptic
/// Gregorian calendar and needs no lookup tables or leap-year special cases.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_formats_correctly() {
        assert_eq!(format_rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn known_instants_format_correctly() {
        // 2026-09-07T12:34:56Z
        assert_eq!(
            format_rfc3339(1_788_784_496, 789),
            "2026-09-07T12:34:56.789Z"
        );
        // A leap day, which a naive month table gets wrong.
        assert_eq!(format_rfc3339(1_709_164_800, 0), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn timestamps_are_fixed_width_so_text_order_matches_time_order() {
        let earlier = format_rfc3339(1_000_000_000, 0);
        let later = format_rfc3339(1_000_000_001, 0);
        assert_eq!(earlier.len(), later.len());
        assert!(earlier < later);
    }

    #[test]
    fn the_system_clock_advances() {
        use tramontana_core::ports::LocalClock;
        let local = SystemLocalClock::new().local_now();
        assert!((1..=12).contains(&local.month) && (1..=31).contains(&local.day));
        assert!(local.hour < 24 && local.minute < 60);
        let clock = SystemClock::new();
        let first = clock.now();
        assert!(first.as_str().ends_with('Z'));
        assert!(clock.now() >= first);
    }
}
