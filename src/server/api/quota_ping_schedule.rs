//! Anchored quota auto-ping schedule (9router `quotaPingSchedule.js` parity).
//!
//! Fires pings at fixed local wall-clock slots instead of (or on top of) the
//! reactive "ping right after reset" behaviour. With anchor 09:30, a 5h window
//! and 3 slots/day the schedule is 09:30 / 14:30 / 19:30 local time — the
//! overnight gap is what re-aligns the next day back onto the 09:30 anchor,
//! since 24h is not a multiple of 5h.
//!
//! Timezone handling uses `chrono-tz` (IANA database) so DST transitions are
//! honoured: wall-clock times are resolved through the same two-pass offset
//! refinement 9router performs with `Intl.DateTimeFormat`.

use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use serde_json::Value;
use std::str::FromStr;

/// Fire a slot that came due while the process was down/busy, but never a stale one.
pub const SLOT_GRACE_MS: i64 = 30 * 60 * 1000;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

pub const DEFAULT_ANCHOR: &str = "09:30";
pub const DEFAULT_WINDOW_HOURS: f64 = 5.0;
pub const DEFAULT_SLOTS_PER_DAY: u32 = 3;

/// A fully-defaulted, validated schedule. `None` from
/// [`normalize_ping_schedule`] means the raw config is absent/disabled/invalid.
#[derive(Clone, Debug, PartialEq)]
pub struct PingSchedule {
    /// `"HH:MM"` 24h wall-clock anchor.
    pub anchor: String,
    /// IANA timezone name (e.g. `"America/New_York"`).
    pub timezone: String,
    pub window_hours: f64,
    pub slots_per_day: u32,
}

#[derive(Clone, Debug)]
pub struct PingSlot {
    /// `"YYYY-MM-DD#<index>"` in the schedule timezone — stable dedup key.
    pub key: String,
    pub index: u32,
    /// UTC epoch milliseconds when the slot fires.
    pub at_ms: i64,
}

/// Host-local IANA timezone (9router `getLocalTimezone` parity — the dashboard
/// computes its own local zone client-side and stores it on the schedule, so
/// this is only the fallback for a missing/invalid `timezone` field).
pub fn local_timezone() -> String {
    iana_time_zone::get_timezone()
        .ok()
        .map(|tz| tz.trim().to_string())
        .filter(|tz| !tz.is_empty())
        .unwrap_or_else(|| "UTC".to_string())
}

/// Parse `"H:MM"` / `"HH:MM"` into `(hour, minute)`; rejects out-of-range.
pub fn parse_anchor(anchor: &str) -> Option<(u32, u32)> {
    let trimmed = anchor.trim();
    let (h, m) = trimmed.split_once(':')?;
    if h.is_empty()
        || m.len() != 2
        || !h.chars().all(|c| c.is_ascii_digit())
        || !m.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some((hour, minute))
}

fn format_anchor(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

/// `floor(24 / windowHours)`, at least 1.
pub fn max_slots_per_day(window_hours: f64) -> u32 {
    if !window_hours.is_finite() || window_hours <= 0.0 {
        return 1;
    }
    (24.0 / window_hours).floor().max(1.0) as u32
}

fn is_valid_timezone(timezone: &str) -> bool {
    Tz::from_str(timezone).is_ok()
}

fn clamp_f64(value: f64, fallback: f64, min: f64, max: f64) -> f64 {
    if !value.is_finite() {
        return fallback;
    }
    value.clamp(min, max)
}

fn number_or(value: Option<&Value>, fallback: f64) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(fallback),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(fallback),
        _ => fallback,
    }
}

/// Returns a fully-defaulted schedule, or `None` when it is
/// absent/disabled/invalid (9router `normalizePingSchedule` parity).
pub fn normalize_ping_schedule(raw: Option<&Value>) -> Option<PingSchedule> {
    let raw = raw?;
    if raw.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }

    let (hour, minute) = raw
        .get("anchor")
        .and_then(|v| v.as_str())
        .and_then(parse_anchor)
        .or_else(|| parse_anchor(DEFAULT_ANCHOR))?;
    let anchor = format_anchor(hour, minute);

    let timezone = raw
        .get("timezone")
        .and_then(|v| v.as_str())
        .filter(|tz| is_valid_timezone(tz))
        .map(str::to_string)
        .unwrap_or_else(local_timezone);

    let window_hours = clamp_f64(
        number_or(raw.get("windowHours"), DEFAULT_WINDOW_HOURS),
        DEFAULT_WINDOW_HOURS,
        0.5,
        24.0,
    );
    let slots_max = max_slots_per_day(window_hours);
    let slots_per_day = number_or(raw.get("slotsPerDay"), DEFAULT_SLOTS_PER_DAY as f64)
        .round()
        .clamp(1.0, slots_max as f64) as u32;

    Some(PingSchedule {
        anchor,
        timezone,
        window_hours,
        slots_per_day,
    })
}

#[derive(Clone, Copy, Debug)]
struct ZonedParts {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

/// Wall-clock parts of `utc_ms` in `tz` (second precision, like
/// `Intl.DateTimeFormat` `formatToParts`).
fn zoned_parts(utc_ms: i64, tz: &Tz) -> Option<ZonedParts> {
    let dt = DateTime::<Utc>::from_timestamp_millis(utc_ms)?;
    let local = dt.with_timezone(tz);
    Some(ZonedParts {
        year: local.year(),
        month: local.month(),
        day: local.day(),
        hour: local.hour(),
        minute: local.minute(),
        second: local.second(),
    })
}

/// `local wall clock read as UTC` minus `utc` — the zone's UTC offset at that
/// instant, in milliseconds (second precision).
fn zone_offset_ms(utc_ms: i64, tz: &Tz) -> Option<i64> {
    let p = zoned_parts(utc_ms, tz)?;
    let wall_as_utc = NaiveDate::from_ymd_opt(p.year, p.month, p.day)?
        .and_hms_opt(p.hour, p.minute, p.second)?
        .and_utc()
        .timestamp_millis();
    // `zoned_parts` is second-precision; floor the instant the same way.
    let utc_s = utc_ms - utc_ms.rem_euclid(1000);
    Some(wall_as_utc - utc_s)
}

/// Wall-clock time in `tz` → UTC epoch ms. The second pass covers DST shifts,
/// mirroring 9router's `zonedWallClockToUtcMs`.
fn zoned_wall_clock_to_utc_ms(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    tz: &Tz,
) -> Option<i64> {
    let naive_ms = NaiveDate::from_ymd_opt(year, month, day)?
        .and_hms_opt(hour, minute, 0)?
        .and_utc()
        .timestamp_millis();
    let offset = zone_offset_ms(naive_ms, tz)?;
    let candidate = naive_ms - offset;
    let refined_offset = zone_offset_ms(candidate, tz)?;
    Some(if refined_offset == offset {
        candidate
    } else {
        naive_ms - refined_offset
    })
}

fn slot_key(anchor_day_ms: i64, tz: &Tz, index: u32) -> Option<String> {
    let p = zoned_parts(anchor_day_ms, tz)?;
    Some(format!("{:04}-{:02}-{:02}#{index}", p.year, p.month, p.day))
}

/// All slots belonging to the anchor day that contains `anchor_day_ms`.
fn slots_for_anchor_day(schedule: &PingSchedule, anchor_day_ms: i64) -> Vec<PingSlot> {
    let tz = match Tz::from_str(&schedule.timezone) {
        Ok(tz) => tz,
        Err(_) => return Vec::new(),
    };
    let Some(p) = zoned_parts(anchor_day_ms, &tz) else {
        return Vec::new();
    };
    let Some((anchor_hour, anchor_minute)) = parse_anchor(&schedule.anchor) else {
        return Vec::new();
    };
    let Some(first_at) =
        zoned_wall_clock_to_utc_ms(p.year, p.month, p.day, anchor_hour, anchor_minute, &tz)
    else {
        return Vec::new();
    };

    (0..schedule.slots_per_day)
        .filter_map(|index| {
            let at =
                first_at + (index as f64 * schedule.window_hours * HOUR_MS as f64).round() as i64;
            slot_key(anchor_day_ms, &tz, index).map(|key| PingSlot {
                key,
                index,
                at_ms: at,
            })
        })
        .collect()
}

/// Slots from yesterday + today + tomorrow cover everything that can already
/// be due, since a day's slots never span more than 24h from its anchor.
pub fn get_schedule_slots(schedule: &PingSchedule, now_ms: i64) -> Vec<PingSlot> {
    let mut slots = Vec::new();
    for delta in [-DAY_MS, 0, DAY_MS] {
        slots.extend(slots_for_anchor_day(schedule, now_ms + delta));
    }
    slots.sort_by_key(|slot| slot.at_ms);
    slots
}

/// Most recent slot that is due and still inside the grace window.
pub fn get_due_ping_slot(schedule: &PingSchedule, now_ms: i64, grace_ms: i64) -> Option<PingSlot> {
    get_schedule_slots(schedule, now_ms)
        .into_iter()
        .filter(|slot| slot.at_ms <= now_ms && now_ms - slot.at_ms <= grace_ms)
        .last()
}

pub fn get_due_ping_slot_default(schedule: &PingSchedule, now_ms: i64) -> Option<PingSlot> {
    get_due_ping_slot(schedule, now_ms, SLOT_GRACE_MS)
}

pub fn get_next_ping_slot(schedule: &PingSchedule, now_ms: i64) -> Option<PingSlot> {
    get_schedule_slots(schedule, now_ms)
        .into_iter()
        .find(|slot| slot.at_ms > now_ms)
}

/// `"09:30 → 14:30 → 19:30"` preview labels for the UI.
pub fn get_schedule_slot_labels(schedule: &PingSchedule) -> Vec<String> {
    let Some((hour, minute)) = parse_anchor(&schedule.anchor) else {
        return Vec::new();
    };
    let start_minutes = hour * 60 + minute;
    (0..schedule.slots_per_day)
        .map(|index| {
            let total = ((start_minutes as f64 + index as f64 * schedule.window_hours * 60.0)
                .round() as i64)
                .rem_euclid(1440);
            format!("{:02}:{:02}", total / 60, total % 60)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schedule(anchor: &str, tz: &str, window: f64, slots: u32) -> PingSchedule {
        PingSchedule {
            anchor: anchor.to_string(),
            timezone: tz.to_string(),
            window_hours: window,
            slots_per_day: slots,
        }
    }

    #[test]
    fn normalize_requires_enabled() {
        assert!(normalize_ping_schedule(Some(&json!({"enabled": false}))).is_none());
        assert!(normalize_ping_schedule(None).is_none());
        assert!(normalize_ping_schedule(Some(&json!({"enabled": true}))).is_some());
    }

    #[test]
    fn normalize_defaults_and_clamps() {
        let s = normalize_ping_schedule(Some(&json!({
            "enabled": true,
            "anchor": "25:99",
            "timezone": "Not/AZone",
            "windowHours": 100,
            "slotsPerDay": 99
        })))
        .unwrap();
        assert_eq!(s.anchor, "09:30"); // invalid anchor falls back to default
        assert_eq!(s.window_hours, 24.0);
        assert_eq!(s.slots_per_day, 1); // max_slots_per_day(24) = 1
        assert!(is_valid_timezone(&s.timezone));
    }

    #[test]
    fn slot_labels_match_upstream_example() {
        let s = schedule("09:30", "UTC", 5.0, 3);
        assert_eq!(
            get_schedule_slot_labels(&s),
            vec!["09:30", "14:30", "19:30"]
        );
    }

    #[test]
    fn slots_are_local_wall_clock_in_utc() {
        // 2026-01-05T12:00:00Z in UTC — anchor 09:30, 5h window, 3 slots.
        let now = DateTime::parse_from_rfc3339("2026-01-05T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let s = schedule("09:30", "UTC", 5.0, 3);
        let slots = get_schedule_slots(&s, now);
        let todays: Vec<_> = slots
            .iter()
            .filter(|slot| slot.key.starts_with("2026-01-05"))
            .collect();
        assert_eq!(todays.len(), 3);
        assert_eq!(
            todays[0].at_ms,
            DateTime::parse_from_rfc3339("2026-01-05T09:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            todays[1].at_ms,
            DateTime::parse_from_rfc3339("2026-01-05T14:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            todays[2].at_ms,
            DateTime::parse_from_rfc3339("2026-01-05T19:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn slots_shift_with_timezone() {
        // Same instant, America/New_York (UTC-5 in January): 07:00 local.
        let now = DateTime::parse_from_rfc3339("2026-01-05T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let s = schedule("09:30", "America/New_York", 5.0, 3);
        let slots = get_schedule_slots(&s, now);
        let todays: Vec<_> = slots
            .iter()
            .filter(|slot| slot.key.starts_with("2026-01-05"))
            .collect();
        // 09:30 EST = 14:30 UTC.
        assert_eq!(
            todays[0].at_ms,
            DateTime::parse_from_rfc3339("2026-01-05T14:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            todays[1].at_ms,
            DateTime::parse_from_rfc3339("2026-01-05T19:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            todays[2].at_ms,
            DateTime::parse_from_rfc3339("2026-01-06T00:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn dst_spring_forward_keeps_wall_clock_anchor() {
        // US DST starts 2026-03-08: 02:00 → 03:00. Anchor 09:30 stays 09:30
        // local on both sides; the UTC instant shifts by an hour.
        let s = schedule("09:30", "America/New_York", 8.0, 3);
        // Now inside the DST day.
        let now = DateTime::parse_from_rfc3339("2026-03-08T18:00:00Z")
            .unwrap()
            .timestamp_millis();
        let slots = get_schedule_slots(&s, now);
        let day = slots
            .iter()
            .find(|slot| slot.key == "2026-03-08#0")
            .expect("slot for 2026-03-08");
        // 09:30 EDT (UTC-4) = 13:30 UTC.
        assert_eq!(
            day.at_ms,
            DateTime::parse_from_rfc3339("2026-03-08T13:30:00Z")
                .unwrap()
                .timestamp_millis()
        );

        // The day before (EST, UTC-5): 09:30 = 14:30 UTC.
        let prev = slots
            .iter()
            .find(|slot| slot.key == "2026-03-07#0")
            .expect("slot for 2026-03-07");
        assert_eq!(
            prev.at_ms,
            DateTime::parse_from_rfc3339("2026-03-07T14:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn dst_fall_back_anchor_in_ambiguous_hour_picks_a_valid_instant() {
        // US DST ends 2026-11-01: 02:00 → 01:00. Anchor 01:30 occurs twice;
        // the schedule must still produce a slot (earliest instant wins).
        let s = schedule("01:30", "America/New_York", 6.0, 4);
        let now = DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let slots = get_schedule_slots(&s, now);
        let day = slots
            .iter()
            .find(|slot| slot.key == "2026-11-01#0")
            .expect("slot for 2026-11-01");
        // 01:30 EDT (first occurrence, UTC-4) = 05:30 UTC.
        assert_eq!(
            day.at_ms,
            DateTime::parse_from_rfc3339("2026-11-01T05:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
        // Slots stay strictly increasing.
        let ats: Vec<i64> = slots.iter().map(|s| s.at_ms).collect();
        assert!(ats.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn nonexistent_wall_time_still_resolves() {
        // 02:30 does not exist on 2026-03-08 in New York (spring forward).
        let s = schedule("02:30", "America/New_York", 1.0, 24);
        let now = DateTime::parse_from_rfc3339("2026-03-08T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let slots = get_schedule_slots(&s, now);
        assert!(!slots.is_empty());
        let ats: Vec<i64> = slots.iter().map(|s| s.at_ms).collect();
        // Upstream JS resolves the nonexistent 02:30 to 06:30Z, which is the
        // same instant as the previous day's last 1h slot — `Intl`-based
        // `zonedWallClockToUtcMs` yields the identical collision, so slots
        // are non-decreasing (never decreasing), not strictly increasing.
        assert!(ats.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn due_slot_respects_grace_window() {
        let s = schedule("09:30", "UTC", 5.0, 3);
        // Exactly at the 14:30 slot → due.
        let at_slot = DateTime::parse_from_rfc3339("2026-01-05T14:30:00Z")
            .unwrap()
            .timestamp_millis();
        let due = get_due_ping_slot(&s, at_slot, SLOT_GRACE_MS).unwrap();
        assert_eq!(due.key, "2026-01-05#1");
        // 31 minutes later → outside grace → none (14:30 + 30min grace).
        let stale = at_slot + SLOT_GRACE_MS + 60_000;
        assert!(get_due_ping_slot(&s, stale, SLOT_GRACE_MS).is_none());
        // Within grace → still due.
        let in_grace = at_slot + SLOT_GRACE_MS - 1;
        assert!(get_due_ping_slot(&s, in_grace, SLOT_GRACE_MS).is_some());
    }

    #[test]
    fn next_slot_is_first_future() {
        let s = schedule("09:30", "UTC", 5.0, 3);
        let now = DateTime::parse_from_rfc3339("2026-01-05T10:00:00Z")
            .unwrap()
            .timestamp_millis();
        let next = get_next_ping_slot(&s, now).unwrap();
        assert_eq!(next.key, "2026-01-05#1");
        assert_eq!(
            next.at_ms,
            DateTime::parse_from_rfc3339("2026-01-05T14:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
    }
}
