#![forbid(unsafe_code)]

//! Schedule types and cron evaluation — E32 S32.2.
//!
//! Three variants cover every scheduling need:
//! - [`JobSchedule::Immediate`] — fires on the next runner poll (one-shot).
//! - [`JobSchedule::Once`] — fires once when `now >= at_ns`.
//! - [`JobSchedule::Cron`] — fires whenever a 5-field cron expression matches.
//!
//! # Cron syntax
//!
//! Supports three per-field forms:
//! - `*` — any value (wildcard).
//! - `N` — exact numeric value.
//! - `*/N` — every N steps from zero (e.g. `*/15` in the minute field = 0, 15, 30, 45).
//!
//! Field order: **minute hour day-of-month month day-of-week**
//! (0–59, 0–23, 1–31, 1–12, 0–6 where 0 = Sunday).

use serde::{Deserialize, Serialize};

// ── JobSchedule ───────────────────────────────────────────────────────────────

/// Controls when a scheduled job fires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobSchedule {
    /// Fire once on the next runner poll (immediate one-shot).
    Immediate,
    /// Fire once when `now_ns >= at_ns`.
    Once {
        /// Unix nanosecond threshold.
        at_ns: u64,
    },
    /// Fire whenever the cron expression matches the current wall-clock minute.
    Cron {
        /// 5-field cron string, e.g. `"0 9 * * 1-5"`.
        expression: String,
    },
}

impl JobSchedule {
    /// Short type label suitable for audit entries and CLI display.
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Once { .. } => "once",
            Self::Cron { .. } => "cron",
        }
    }

    /// Returns `true` if the schedule is due to fire.
    ///
    /// `now_ns` is the current Unix nanosecond timestamp.
    /// `last_fired_ns` is the timestamp of the previous firing (0 = never fired).
    pub fn is_due(&self, now_ns: u64, last_fired_ns: u64) -> bool {
        match self {
            Self::Immediate => last_fired_ns == 0,
            Self::Once { at_ns } => now_ns >= *at_ns && last_fired_ns == 0,
            Self::Cron { expression } => is_cron_due(expression, now_ns, last_fired_ns),
        }
    }
}

// ── Cron evaluation ───────────────────────────────────────────────────────────

/// Returns `true` when `expression` matches `now_ns` and the job has not
/// already fired during the current minute (guarded by `last_fired_ns`).
pub fn is_cron_due(expression: &str, now_ns: u64, last_fired_ns: u64) -> bool {
    let secs = now_ns / 1_000_000_000;
    let (minute, hour, day, month, weekday) = decompose_unix_secs(secs);

    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }

    if !field_matches(parts[0], minute as u64)
        || !field_matches(parts[1], hour as u64)
        || !field_matches(parts[2], day as u64)
        || !field_matches(parts[3], month as u64)
        || !field_matches(parts[4], weekday as u64)
    {
        return false;
    }

    // At most one firing per minute: last_fired_ns must be before the start of
    // the current minute.
    let current_minute_start_ns = (secs / 60) * 60 * 1_000_000_000_u64;
    last_fired_ns < current_minute_start_ns
}

/// Returns `true` when `field` (one of `*`, `N`, or `*/N`) matches `value`.
fn field_matches(field: &str, value: u64) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step_str) = field.strip_prefix("*/") {
        let n: u64 = match step_str.parse() {
            Ok(n) if n > 0 => n,
            _ => return false,
        };
        return value.is_multiple_of(n);
    }
    field.parse::<u64>() == Ok(value)
}

/// Decomposes a Unix seconds timestamp into `(minute, hour, day, month, weekday)`.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm (public domain) for the
/// Gregorian calendar conversion.  Weekday: Sunday = 0 (1970-01-01 was a
/// Thursday = 4).
fn decompose_unix_secs(unix_secs: u64) -> (u8, u8, u8, u8, u8) {
    let days = unix_secs / 86400;
    let time_of_day = (unix_secs % 86400) as u32;

    let hour = (time_of_day / 3600) as u8;
    let minute = ((time_of_day % 3600) / 60) as u8;
    let weekday = ((days + 4) % 7) as u8; // Thu=4 on 1970-01-01

    // Gregorian calendar conversion (civil_from_days, Howard Hinnant)
    let z = days as i64 + 719_468;
    let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 {
        (mp + 3) as u8
    } else {
        (mp - 9) as u8
    };

    (minute, hour, day, month, weekday)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // 2024-01-15 09:30:00 UTC = 1705311000 seconds (verified: minute=30, hour=9, weekday=1 Monday)
    // 2024-01-15 09:30:45 UTC = 1705311045 seconds (45 s into the same minute)
    // 2024-01-15 09:45:00 UTC = 1705311900 seconds (minute=45)
    const T_09_30_00: u64 = 1_705_311_000 * 1_000_000_000; // Mon 09:30:00
    const T_09_30_45: u64 = 1_705_311_045 * 1_000_000_000; // Mon 09:30:45 (mid-minute)
    const T_09_45_00: u64 = 1_705_311_900 * 1_000_000_000; // Mon 09:45:00

    // Keep these aliases for readability in the tests below.
    const T_2024_01_15_09_30: u64 = T_09_30_00;
    const T_2024_01_15_09_45: u64 = T_09_45_00;

    #[test]
    fn immediate_is_due_when_never_fired() {
        let s = JobSchedule::Immediate;
        assert!(s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn immediate_not_due_when_already_fired() {
        let s = JobSchedule::Immediate;
        assert!(!s.is_due(T_2024_01_15_09_30, 1));
    }

    #[test]
    fn once_is_due_when_time_has_passed() {
        let at = T_2024_01_15_09_30 - 1_000_000_000; // 1 s in the past
        let s = JobSchedule::Once { at_ns: at };
        assert!(s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn once_not_due_before_its_time() {
        let at = T_2024_01_15_09_30 + 60_000_000_000; // 60 s in the future
        let s = JobSchedule::Once { at_ns: at };
        assert!(!s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn once_not_due_when_already_fired() {
        let at = T_2024_01_15_09_30 - 1_000_000_000;
        let s = JobSchedule::Once { at_ns: at };
        // last_fired non-zero means it already ran
        assert!(!s.is_due(T_2024_01_15_09_30, T_2024_01_15_09_30 - 2_000_000_000));
    }

    #[test]
    fn cron_wildcard_matches_any_minute() {
        // "* * * * *" should fire every minute
        let s = JobSchedule::Cron {
            expression: "* * * * *".to_owned(),
        };
        // never fired → due
        assert!(s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn cron_exact_minute_matches() {
        // "30 9 * * *" — minute=30, hour=9
        let s = JobSchedule::Cron {
            expression: "30 9 * * *".to_owned(),
        };
        assert!(s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn cron_wrong_minute_does_not_fire() {
        // "45 9 * * *" — doesn't match minute=30
        let s = JobSchedule::Cron {
            expression: "45 9 * * *".to_owned(),
        };
        assert!(!s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn cron_not_due_when_fired_in_same_minute() {
        // now = 09:30:45, fired_at = 09:30:00 (same minute → must not re-fire)
        let s = JobSchedule::Cron {
            expression: "* * * * *".to_owned(),
        };
        assert!(!s.is_due(T_09_30_45, T_09_30_00));
    }

    #[test]
    fn cron_due_when_fired_in_prior_minute() {
        // now = 09:30:45, fired_at = 09:29:15 (different minute → should fire)
        let s = JobSchedule::Cron {
            expression: "* * * * *".to_owned(),
        };
        let fired_last_minute = T_09_30_45 - 90_000_000_000_u64; // 1.5 min before
        assert!(s.is_due(T_09_30_45, fired_last_minute));
    }

    #[test]
    fn cron_step_matches_every_15_minutes() {
        // "*/15 * * * *" — fires at minutes 0, 15, 30, 45
        let s = JobSchedule::Cron {
            expression: "*/15 * * * *".to_owned(),
        };
        // T_2024_01_15_09_30 is minute=30 → 30 % 15 == 0 → matches
        assert!(s.is_due(T_2024_01_15_09_30, 0));
        // T_2024_01_15_09_45 is minute=45 → 45 % 15 == 0 → matches
        assert!(s.is_due(T_2024_01_15_09_45, 0));
    }

    #[test]
    fn cron_invalid_expression_never_fires() {
        let s = JobSchedule::Cron {
            expression: "not-a-cron".to_owned(),
        };
        assert!(!s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn cron_wrong_hour_does_not_fire() {
        // "30 10 * * *" — hour must be 10, but it's 09
        let s = JobSchedule::Cron {
            expression: "30 10 * * *".to_owned(),
        };
        assert!(!s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn type_labels_are_correct() {
        assert_eq!(JobSchedule::Immediate.type_label(), "immediate");
        assert_eq!(JobSchedule::Once { at_ns: 0 }.type_label(), "once");
        assert_eq!(
            JobSchedule::Cron {
                expression: "* * * * *".to_owned()
            }
            .type_label(),
            "cron"
        );
    }

    #[test]
    fn field_matches_wildcard() {
        assert!(field_matches("*", 0));
        assert!(field_matches("*", 59));
    }

    #[test]
    fn field_matches_exact_value() {
        assert!(field_matches("30", 30));
        assert!(!field_matches("30", 31));
    }

    #[test]
    fn field_matches_step_zero() {
        assert!(field_matches("*/5", 0));
        assert!(field_matches("*/5", 5));
        assert!(field_matches("*/5", 30));
        assert!(!field_matches("*/5", 3));
    }

    #[test]
    fn field_matches_rejects_zero_step() {
        assert!(!field_matches("*/0", 0));
    }

    #[test]
    fn decompose_known_timestamp() {
        // 2024-01-15 09:30:00 UTC = 1705311000 — Monday = weekday 1
        let (minute, hour, day, month, weekday) = decompose_unix_secs(1_705_311_000);
        assert_eq!(minute, 30);
        assert_eq!(hour, 9);
        assert_eq!(day, 15);
        assert_eq!(month, 1);
        assert_eq!(weekday, 1); // Monday
    }
}
