//! DATES FROM SECONDS AND BACK (they lived in the licence module until the licence was removed, 2026-11-27: antiloki is free and open source).
const DAY: i64 = 24 * 3600;

/// "2025-09-17T12:00:00.000000Z" → unix secs. Hand-rolled: no chrono in this crate, and the
/// provider's format is fixed. Anything unparsable is `None` (treated as "never expires" only
/// together with a `lifetime` variant — see `plan_of`).
/// unix secs → `YYYY-MM-DD` (civil from days, Howard Hinnant). Pure.
pub fn iso_day(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468; let era = z.div_euclid(146_097); let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // days from civil (Howard Hinnant), proleptic Gregorian
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * DAY + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_day_and_a_stamp_read_both_ways() {
        assert_eq!(iso_day(1_789_948_800), "2026-09-21"); assert_eq!(iso_day(0), "1970-01-01"); assert_eq!(iso_day(951_782_400), "2000-02-29");
        assert_eq!(parse_iso("1970-01-02T00:00:00.000000Z"), Some(DAY)); assert_eq!(parse_iso("2027-03-01T12:30:15Z"), Some(1803904215)); assert_eq!(parse_iso("nope"), None);
    }
}
