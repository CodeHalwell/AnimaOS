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
//! Supports the following per-field forms (standard cron semantics):
//! - `*` — any value (wildcard).
//! - `N` — exact numeric value.
//! - `A-B` — inclusive range (e.g. `1-5` = 1,2,3,4,5).
//! - `A,B,C` — list of values and/or ranges (e.g. `1,3,5` or `0,9-17`).
//! - `*/N` — every N steps from zero (e.g. `*/15` in the minute field = 0, 15, 30, 45).
//! - `A-B/N` — every N steps across a range (e.g. `0-30/10` = 0, 10, 20, 30).
//!
//! Field order: **minute hour day-of-month month day-of-week**
//! (0–59, 0–23, 1–31, 1–12, 0–6 where 0 = Sunday).
//!
//! # Time zone
//!
//! Cron expressions are evaluated in **UTC**. There is no time-zone support;
//! `09:00` in a cron expression means 09:00 UTC regardless of the host's local
//! time zone.

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
    ///
    /// Cron expressions are evaluated in **UTC** (no time-zone support).
    Cron {
        /// 5-field cron string, e.g. `"0 9 * * 1-5"` (09:00 UTC, Mon–Fri).
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
///
/// A malformed expression always returns `false` (defensive fallback);
/// expressions should be rejected at creation time via [`validate_cron`].
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

/// Validates a 5-field cron expression, returning a human-readable error
/// describing the first problem found.
///
/// This is intended to be called at job-creation time so a malformed
/// expression is rejected loudly instead of being persisted and silently
/// never firing. The accepted per-field forms are documented on the
/// [module](crate::schedule); each field must consist of `*`, `N`, `A-B`,
/// `A,B,C`, `*/N`, or `A-B/N` combinations.
///
/// Numeric field values are bounds-checked against their standard domains so
/// a typo like `0 25 * * *` (hour 25) is rejected at creation rather than
/// silently never firing. Structurally invalid fields (non-numeric, zero
/// step, reversed range) are rejected too.
pub fn validate_cron(expression: &str) -> Result<(), String> {
    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(format!(
            "cron expression must have exactly 5 fields, found {}: {expression:?}",
            parts.len()
        ));
    }
    // (label, inclusive min, inclusive max) per standard cron field.
    let fields = [
        ("minute", 0, 59),
        ("hour", 0, 23),
        ("day-of-month", 1, 31),
        ("month", 1, 12),
        ("day-of-week", 0, 6),
    ];
    for (field, (label, min, max)) in parts.iter().zip(fields) {
        validate_field(field, min, max)
            .map_err(|e| format!("invalid {label} field {field:?}: {e}"))?;
    }
    Ok(())
}

/// Validates a single cron field against the `[min, max]` domain.
fn validate_field(field: &str, min: u64, max: u64) -> Result<(), String> {
    if field.is_empty() {
        return Err("empty field".to_owned());
    }
    // A field is a comma-separated list of items; each item is `*`, `N`,
    // `A-B`, `*/N`, or `A-B/N`.
    for item in field.split(',') {
        validate_item(item, min, max)?;
    }
    Ok(())
}

/// Validates a single comma-separated item within a cron field.
fn validate_item(item: &str, min: u64, max: u64) -> Result<(), String> {
    if item.is_empty() {
        return Err("empty list element".to_owned());
    }

    // Split an optional `/N` step suffix.
    let (base, step) = match item.split_once('/') {
        Some((base, step_str)) => {
            match step_str.parse::<u64>() {
                Ok(n) if n > 0 => {}
                _ => return Err(format!("step must be a positive integer in {item:?}")),
            }
            (base, true)
        }
        None => (item, false),
    };

    if base == "*" {
        return Ok(());
    }

    let in_bounds = |n: u64| -> Result<u64, String> {
        if n < min || n > max {
            Err(format!("value {n} is out of bounds ({min}-{max})"))
        } else {
            Ok(n)
        }
    };

    // The base is either a single value `N` or a range `A-B`.
    match base.split_once('-') {
        Some((a, b)) => {
            let a: u64 = a
                .parse()
                .map_err(|_| format!("range start {a:?} is not a number"))?;
            let b: u64 = b
                .parse()
                .map_err(|_| format!("range end {b:?} is not a number"))?;
            if a > b {
                return Err(format!("range start {a} is greater than end {b}"));
            }
            in_bounds(a)?;
            in_bounds(b)?;
            Ok(())
        }
        None => {
            if step {
                // `N/M` without a range is not valid cron; require `*` or a range.
                return Err(format!(
                    "step requires `*` or a range before `/` in {item:?}"
                ));
            }
            let n = base
                .parse::<u64>()
                .map_err(|_| format!("{base:?} is not a number"))?;
            in_bounds(n)?;
            Ok(())
        }
    }
}

/// Returns `true` when `field` matches `value`.
///
/// Supports `*`, `N`, `A-B`, `A,B,C` (lists of values/ranges), `*/N`, and
/// `A-B/N`. Returns `false` for any structurally invalid field.
fn field_matches(field: &str, value: u64) -> bool {
    field.split(',').any(|item| item_matches(item, value))
}

/// Returns `true` when a single comma-separated `item` matches `value`.
fn item_matches(item: &str, value: u64) -> bool {
    // Split optional `/N` step.
    let (base, step) = match item.split_once('/') {
        Some((base, step_str)) => match step_str.parse::<u64>() {
            Ok(n) if n > 0 => (base, Some(n)),
            _ => return false,
        },
        None => (item, None),
    };

    // Determine the inclusive [lo, hi] range the base covers.
    let (lo, hi) = if base == "*" {
        // Unbounded wildcard: anchor stepping at zero.
        (0, u64::MAX)
    } else if let Some((a, b)) = base.split_once('-') {
        match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(a), Ok(b)) if a <= b => (a, b),
            _ => return false,
        }
    } else {
        // Single value: a bare `N` (no step) matches exactly; `N/M` is invalid.
        return match base.parse::<u64>() {
            Ok(n) => step.is_none() && n == value,
            Err(_) => false,
        };
    };

    if value < lo || value > hi {
        return false;
    }
    match step {
        Some(n) => (value - lo).is_multiple_of(n),
        None => true,
    }
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
    fn field_matches_range() {
        // "1-5" matches 1,2,3,4,5 but not 0 or 6.
        assert!(!field_matches("1-5", 0));
        for v in 1..=5 {
            assert!(field_matches("1-5", v), "expected {v} to match 1-5");
        }
        assert!(!field_matches("1-5", 6));
    }

    #[test]
    fn field_matches_list() {
        // "1,3,5" matches exactly those values.
        assert!(field_matches("1,3,5", 1));
        assert!(field_matches("1,3,5", 3));
        assert!(field_matches("1,3,5", 5));
        assert!(!field_matches("1,3,5", 2));
        assert!(!field_matches("1,3,5", 4));
    }

    #[test]
    fn field_matches_list_of_ranges_and_values() {
        // "0,9-17" matches 0 and 9..=17.
        assert!(field_matches("0,9-17", 0));
        assert!(field_matches("0,9-17", 9));
        assert!(field_matches("0,9-17", 17));
        assert!(!field_matches("0,9-17", 8));
        assert!(!field_matches("0,9-17", 18));
        assert!(!field_matches("0,9-17", 1));
    }

    #[test]
    fn field_matches_step_over_range() {
        // "0-30/10" matches 0,10,20,30.
        for v in [0, 10, 20, 30] {
            assert!(field_matches("0-30/10", v), "expected {v} to match 0-30/10");
        }
        assert!(!field_matches("0-30/10", 5));
        assert!(!field_matches("0-30/10", 40)); // outside the range
    }

    #[test]
    fn field_matches_rejects_reversed_range() {
        assert!(!field_matches("5-1", 3));
    }

    #[test]
    fn cron_weekday_range_matches_documented_example() {
        // "0 9 * * 1-5" — 09:00 UTC Mon–Fri. T_09_30 is Monday (weekday 1).
        // Use a 09:00 timestamp to match the minute=0 field.
        // 2024-01-15 09:00:00 UTC = 1705309200.
        let nine_am = 1_705_309_200_u64 * 1_000_000_000;
        let s = JobSchedule::Cron {
            expression: "0 9 * * 1-5".to_owned(),
        };
        assert!(
            s.is_due(nine_am, 0),
            "Monday 09:00 should match 0 9 * * 1-5"
        );
    }

    #[test]
    fn cron_minute_list_matches() {
        // "0,30 9 * * *" should match minute 30.
        let s = JobSchedule::Cron {
            expression: "0,30 9 * * *".to_owned(),
        };
        assert!(s.is_due(T_2024_01_15_09_30, 0));
    }

    #[test]
    fn validate_cron_accepts_valid_expressions() {
        for expr in [
            "* * * * *",
            "30 9 * * *",
            "0 9 * * 1-5",
            "0,30 9 * * *",
            "*/15 * * * *",
            "0-30/10 * * * *",
            "0 9-17 * * 1,3,5",
        ] {
            assert!(validate_cron(expr).is_ok(), "expected {expr:?} to validate");
        }
    }

    #[test]
    fn validate_cron_rejects_wrong_field_count() {
        assert!(validate_cron("* * * *").is_err());
        assert!(validate_cron("* * * * * *").is_err());
        assert!(validate_cron("").is_err());
    }

    #[test]
    fn validate_cron_rejects_malformed_fields() {
        assert!(validate_cron("not-a-cron word here now").is_err());
        assert!(validate_cron("60 abc * * *").is_err());
        assert!(validate_cron("*/0 * * * *").is_err()); // zero step
        assert!(validate_cron("5-1 * * * *").is_err()); // reversed range
        assert!(validate_cron("1,,3 * * * *").is_err()); // empty list element
        assert!(validate_cron("5/2 * * * *").is_err()); // step without range/wildcard
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
