//! Scheduler — pure parsing + tick-decision helpers, lifted from Odin's
//! `flow_scheduler.rs` (Sprint 055). This crate owns the scheduler loop; the
//! Odin copy stays for backward-compat of the tests but is no longer spawned.
//!
//! Kept deliberately small and dependency-free so it can be unit-tested
//! without spinning up a tokio runtime, HTTP clients, or the full config
//! surface.

use chrono::{DateTime, Local};
use cron::Schedule as CronSchedule;

#[derive(Debug)]
#[allow(dead_code)] // Interval's u64 is consumed via pattern match in tests only.
pub enum ScheduleKind {
    /// Legacy `"every_Nm"` form — fires every N minutes.
    Interval(u64),
    /// Full cron expression (5 or 6 fields accepted by the `cron` crate).
    Cron(CronSchedule),
}

/// Parse a cron expression string.
///
/// The `cron` crate accepts both 5-field (standard Unix cron) and 6-field
/// (with seconds) expressions. We accept either: if a 5-field expression
/// is supplied we prepend `"0 "` to force second=0, matching Unix semantics
/// where `"0 3 * * *"` means "at 03:00:00".
pub fn parse_cron_expression(s: &str) -> Option<CronSchedule> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let field_count = trimmed.split_whitespace().count();
    let normalised = match field_count {
        5 => format!("0 {trimmed}"),
        6 | 7 => trimmed.to_string(),
        _ => return None,
    };
    normalised.parse::<CronSchedule>().ok()
}

/// Parse a schedule string into an `Interval` or `Cron` kind.
///
/// Dispatch rules:
/// - Starts with `"every_"` AND ends with `'m'` → `Interval(N)`.
/// - Contains a space (so has multiple fields) → `Cron(schedule)`.
/// - Anything else → `None` (the dispatcher warn-logs and skips the flow).
pub fn parse_schedule(schedule: &str) -> Option<ScheduleKind> {
    let s = schedule.trim();
    if s.starts_with("every_") && s.ends_with('m') {
        return s[6..s.len() - 1].parse::<u64>().ok().map(ScheduleKind::Interval);
    }
    if s.contains(' ') {
        return parse_cron_expression(s).map(ScheduleKind::Cron);
    }
    None
}

/// Decide whether a cron flow should fire on this tick.
///
/// Returns `Some(scheduled_time)` when the next scheduled instant after
/// `last_fire` is `<= now`. The caller MUST advance `last_fire` to the
/// returned `scheduled_time` on fire, so the next tick asks for the
/// *next* future instant — preventing DST-fallback double-fires and
/// tick-jitter duplicate fires.
pub fn should_fire(
    schedule: &CronSchedule,
    last_fire: DateTime<Local>,
    now: DateTime<Local>,
) -> Option<DateTime<Local>> {
    let next = schedule.after(&last_fire).next()?;
    if next <= now {
        Some(next)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_interval_form() {
        assert!(matches!(parse_schedule("every_60m"), Some(ScheduleKind::Interval(60))));
        assert!(matches!(parse_schedule("every_5m"), Some(ScheduleKind::Interval(5))));
    }

    #[test]
    fn parses_five_field_cron() {
        assert!(matches!(parse_schedule("0 3 * * *"), Some(ScheduleKind::Cron(_))));
    }

    #[test]
    fn parses_six_field_cron() {
        assert!(matches!(parse_schedule("0 0 3 * * *"), Some(ScheduleKind::Cron(_))));
    }

    #[test]
    fn rejects_empty_and_gibberish() {
        assert!(parse_schedule("").is_none());
        assert!(parse_schedule("every_xm").is_none());
        assert!(parse_schedule("notacron").is_none());
    }

    #[test]
    fn should_fire_returns_none_in_future() {
        let schedule: CronSchedule = "0 0 0 1 1 *".parse().unwrap(); // Jan 1 midnight yearly
        let now = Local::now();
        let last = now;
        // Next Jan 1 midnight is in the future.
        assert!(should_fire(&schedule, last, now).is_none());
    }
}
