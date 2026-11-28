//! schedule-math.rs — twin of skills/bugfinder/_helpers/schedule-math.mjs (no-db helper). NAMES ARE
//! CONTRACT: computeNextDue · isOverdue. Pure UTC due-date math; cadence 'manual' never becomes due.
// The `now` default (`new Date()`) is frozen by the conform lane to 2026-01-01T00:00:00.000Z, and the
// harness never passes `now` (a JSON value there would make the JS `now.toISOString()` throw), so the
// twin uses that frozen instant directly. All date arithmetic mirrors JS Date.UTC / toISOString.
use serde_json::{json, Value};

fn now_iso() -> String { crate::clock::now_iso_nodb() }
// getUTC{FullYear,Month(0-based),Date} of the frozen instant: 2026, 0 (January), 1.
const FROZEN_Y: i64 = 2026;
const FROZEN_MO: i64 = 0;
const FROZEN_D: i64 = 1;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// Number.isInteger(v): Some(int) if v is a JSON number with no fractional part; else None.
fn js_integer(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => {
            let f = n.as_f64()?;
            if f.is_finite() && f.fract() == 0.0 { Some(f as i64) } else { None }
        }
        _ => None,
    }
}

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// JS `new Date(ms).toISOString()` for our range: 4-digit year, milliseconds always .000 here.
fn iso_from_ms(ms: i64) -> String {
    let day = ms.div_euclid(86_400_000);
    let mut rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(day);
    let h = rem / 3_600_000; rem %= 3_600_000;
    let mi = rem / 60_000; rem %= 60_000;
    let s = rem / 1000; let mmm = rem % 1000;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, m, d, h, mi, s, mmm)
}

// anchor.getUTC{FullYear,Month,Date}: read the calendar date of the (UTC-zoned) ISO string. JS parses
// a 'T' string as-is or SQL 'YYYY-MM-DD HH:MM:SS' → 'YYYY-MM-DDTHH:MM:SSZ'; either way the date
// segment before 'T'/' ' is the UTC date. Month returned 0-based to mirror getUTCMonth.
fn anchor_ymd(iso: &str) -> Option<(i64, i64, i64)> {
    let date_part = iso.split(|c| c == 'T' || c == ' ').next()?;
    let mut it = date_part.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let mo: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    Some((y, mo - 1, d))
}

pub fn computeNextDue(schedule: &Value, _now: &Value) -> Value {
    let cadence = schedule.get("cadence").and_then(|c| c.as_str());
    if !truthy(schedule.get("enabled")) || cadence == Some("manual") { return Value::Null; }
    let step_days: i64 = if cadence == Some("daily") { 1 } else { 7 };
    let hour = js_integer(schedule.get("hour_utc")).unwrap_or(6);
    let anchor_iso = nz(schedule.get("last_run_at")).or_else(|| nz(schedule.get("created_at")));
    let (ay, amo, ad) = match anchor_iso {
        Some(v) if truthy(Some(v)) => anchor_ymd(v.as_str().unwrap_or("")).unwrap_or((FROZEN_Y, FROZEN_MO, FROZEN_D)),
        _ => (FROZEN_Y, FROZEN_MO, FROZEN_D),
    };
    // Date.UTC(ay, amo, ad + step_days, hour, 0, 0): amo is 0..11, so day = first-of-month + (dt-1).
    let day_number = days_from_civil(ay, amo + 1, 1) + (ad + step_days - 1);
    let ms = day_number * 86_400_000 + hour * 3_600_000;
    json!(iso_from_ms(ms))
}

pub fn isOverdue(schedule: &Value, now: &Value) -> Value {
    match computeNextDue(schedule, now).as_str() {
        Some(due) => json!(due <= now_iso().as_str()),
        None => json!(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cnd(s: Value) -> Value { computeNextDue(&s, &Value::Null) }
    fn od(s: Value) -> Value { isOverdue(&s, &Value::Null) }

    #[test]
    fn compute_next_due() {
        assert_eq!(cnd(Value::Null), Value::Null);
        assert_eq!(cnd(json!({ "enabled": false, "cadence": "daily" })), Value::Null);
        assert_eq!(cnd(json!({ "cadence": "daily" })), Value::Null);
        assert_eq!(cnd(json!({ "enabled": true, "cadence": "manual", "created_at": "2025-12-20T06:00:00.000Z" })), Value::Null);
        assert_eq!(cnd(json!({ "enabled": true, "cadence": "daily", "hour_utc": 6, "last_run_at": "2025-12-20T06:00:00.000Z" })), json!("2025-12-21T06:00:00.000Z"));
        // SQL-format anchor (no 'T'), created_at fallback, default hour 6, weekly
        assert_eq!(cnd(json!({ "enabled": true, "cadence": "weekly", "created_at": "2025-12-20 09:30:00" })), json!("2025-12-27T06:00:00.000Z"));
        // string hour_utc is not an integer → default 6; +7 days rolls into next year
        assert_eq!(cnd(json!({ "enabled": true, "cadence": "weekly", "hour_utc": "8", "last_run_at": "2025-12-28T00:00:00.000Z" })), json!("2026-01-04T06:00:00.000Z"));
        // no anchor → frozen now (2026-01-01); hour 0
        assert_eq!(cnd(json!({ "enabled": true, "cadence": "daily", "hour_utc": 0 })), json!("2026-01-02T00:00:00.000Z"));
        // day + year rollover (Dec 31 + 1)
        assert_eq!(cnd(json!({ "enabled": true, "cadence": "daily", "hour_utc": 23, "last_run_at": "2025-12-31T10:00:00.000Z" })), json!("2026-01-01T23:00:00.000Z"));
        // negative hour normalizes back a day
        assert_eq!(cnd(json!({ "enabled": true, "cadence": "daily", "hour_utc": -1, "last_run_at": "2026-01-01T12:00:00.000Z" })), json!("2026-01-01T23:00:00.000Z"));
    }

    #[test]
    fn is_overdue() {
        assert_eq!(od(json!({ "enabled": true, "cadence": "manual", "created_at": "2025-12-20T06:00:00.000Z" })), json!(false));
        assert_eq!(od(json!({ "enabled": true, "cadence": "daily", "hour_utc": 6, "last_run_at": "2025-12-20T06:00:00.000Z" })), json!(true));
        assert_eq!(od(json!({ "enabled": true, "cadence": "daily", "hour_utc": 6, "last_run_at": "2026-06-01T06:00:00.000Z" })), json!(false));
        // due == now exactly → overdue (<=)
        assert_eq!(od(json!({ "enabled": true, "cadence": "daily", "hour_utc": 0, "last_run_at": "2025-12-31T05:00:00.000Z" })), json!(true));
    }
}
