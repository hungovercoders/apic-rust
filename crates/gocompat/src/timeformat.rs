//! Formatting a [`Time`] with a reference-time layout: the layout shows
//! how `Mon Jan 2 15:04:05 MST 2006` should look (`2006-01-02`,
//! `02 Jan 06 15:04`), and the time is written the same way. This is the
//! format `{{$datetime "2006-01-02"}}` takes, so the layouts in existing
//! request files keep their meaning.

use crate::time::Time;

/// The layout of RFC 3339 without a fraction: `2006-01-02T15:04:05Z07:00`.
pub const RFC3339: &str = "2006-01-02T15:04:05Z07:00";
/// The layout of RFC 1123 with a zone name: `Mon, 02 Jan 2006 15:04:05 MST`.
pub const RFC1123: &str = "Mon, 02 Jan 2006 15:04:05 MST";

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Chunk {
    LongMonth,
    Month,
    NumMonth,
    ZeroMonth,
    LongWeekDay,
    WeekDay,
    Day,
    UnderDay,
    ZeroDay,
    UnderYearDay,
    ZeroYearDay,
    Hour,
    Hour12,
    ZeroHour12,
    Minute,
    ZeroMinute,
    Second,
    ZeroSecond,
    LongYear,
    Year,
    Pm,
    PmLower,
    Tz,
    Iso8601Tz,
    Iso8601SecondsTz,
    Iso8601ShortTz,
    Iso8601ColonTz,
    Iso8601ColonSecondsTz,
    NumTz,
    NumSecondsTz,
    NumShortTz,
    NumColonTz,
    NumColonSecondsTz,
    /// `.000` or `,000`: a fixed number of digits.
    FracFixed(usize),
    /// `.999` or `,999`: up to that many digits, trailing zeros dropped.
    FracTrim(usize),
}

/// Finds the next layout element in `layout`: the text before it, the
/// element, and the text after it.
fn next_chunk(layout: &str) -> (&str, Option<Chunk>, &str) {
    let b = layout.as_bytes();
    let found = |i: usize, len: usize, c: Chunk| (&layout[..i], Some(c), &layout[i + len..]);
    for i in 0..b.len() {
        let rest = &layout[i..];
        match b[i] {
            b'J' => {
                if rest.starts_with("January") {
                    return found(i, 7, Chunk::LongMonth);
                }
                if rest.starts_with("Jan") {
                    return found(i, 3, Chunk::Month);
                }
            }
            b'M' => {
                if rest.starts_with("Monday") {
                    return found(i, 6, Chunk::LongWeekDay);
                }
                if rest.starts_with("Mon") {
                    return found(i, 3, Chunk::WeekDay);
                }
                if rest.starts_with("MST") {
                    return found(i, 3, Chunk::Tz);
                }
            }
            b'0' => {
                if rest.len() >= 2 && (b'1'..=b'6').contains(&b[i + 1]) {
                    let c = match b[i + 1] {
                        b'1' => Chunk::ZeroMonth,
                        b'2' => Chunk::ZeroDay,
                        b'3' => Chunk::ZeroHour12,
                        b'4' => Chunk::ZeroMinute,
                        b'5' => Chunk::ZeroSecond,
                        _ => Chunk::Year,
                    };
                    return found(i, 2, c);
                }
                if rest.starts_with("002") {
                    return found(i, 3, Chunk::ZeroYearDay);
                }
            }
            b'1' => {
                if rest.starts_with("15") {
                    return found(i, 2, Chunk::Hour);
                }
                return found(i, 1, Chunk::NumMonth);
            }
            b'2' => {
                if rest.starts_with("2006") {
                    return found(i, 4, Chunk::LongYear);
                }
                return found(i, 1, Chunk::Day);
            }
            b'_' => {
                if rest.starts_with("_2006") {
                    // `_2006` is a literal underscore then the year.
                    return found(i + 1, 4, Chunk::LongYear);
                }
                if rest.starts_with("__2") {
                    return found(i, 3, Chunk::UnderYearDay);
                }
                if rest.starts_with("_2") {
                    return found(i, 2, Chunk::UnderDay);
                }
            }
            b'3' => return found(i, 1, Chunk::Hour12),
            b'4' => return found(i, 1, Chunk::Minute),
            b'5' => return found(i, 1, Chunk::Second),
            b'P' => {
                if rest.starts_with("PM") {
                    return found(i, 2, Chunk::Pm);
                }
            }
            b'p' => {
                if rest.starts_with("pm") {
                    return found(i, 2, Chunk::PmLower);
                }
            }
            b'-' => {
                for (text, c) in [
                    ("-070000", Chunk::NumSecondsTz),
                    ("-07:00:00", Chunk::NumColonSecondsTz),
                    ("-0700", Chunk::NumTz),
                    ("-07:00", Chunk::NumColonTz),
                    ("-07", Chunk::NumShortTz),
                ] {
                    if rest.starts_with(text) {
                        return found(i, text.len(), c);
                    }
                }
            }
            b'Z' => {
                for (text, c) in [
                    ("Z070000", Chunk::Iso8601SecondsTz),
                    ("Z07:00:00", Chunk::Iso8601ColonSecondsTz),
                    ("Z0700", Chunk::Iso8601Tz),
                    ("Z07:00", Chunk::Iso8601ColonTz),
                    ("Z07", Chunk::Iso8601ShortTz),
                ] {
                    if rest.starts_with(text) {
                        return found(i, text.len(), c);
                    }
                }
            }
            b'.' | b',' if i + 1 < b.len() && (b[i + 1] == b'0' || b[i + 1] == b'9') => {
                // A fraction of a second: a run of 0s or of 9s that no
                // other digit follows.
                let digit = b[i + 1];
                let mut j = i + 1;
                while j < b.len() && b[j] == digit {
                    j += 1;
                }
                if !(j < b.len() && b[j].is_ascii_digit()) {
                    let n = j - (i + 1);
                    let c = if digit == b'0' {
                        Chunk::FracFixed(n)
                    } else {
                        Chunk::FracTrim(n)
                    };
                    return (&layout[..i + 1], Some(c), &layout[j..]);
                }
            }
            _ => {}
        }
    }
    (layout, None, "")
}

/// The day of the week of a date, 0 for Sunday.
fn weekday(days_since_epoch: i64) -> usize {
    (days_since_epoch + 4).rem_euclid(7) as usize
}

fn zone_offset(out: &mut String, offset: i32, colon: bool, seconds: bool, short: bool) {
    let mut zone = offset;
    if zone < 0 {
        out.push('-');
        zone = -zone;
    } else {
        out.push('+');
    }
    out.push_str(&format!("{:02}", zone / 3600));
    if short {
        return;
    }
    if colon {
        out.push(':');
    }
    out.push_str(&format!("{:02}", zone % 3600 / 60));
    if seconds {
        if colon {
            out.push(':');
        }
        out.push_str(&format!("{:02}", zone % 60));
    }
}

/// Writes `t`, in its own offset, the way `layout` shows the reference
/// time. `zone` is the abbreviation of the time's zone for layouts that
/// print one (`MST`): `UTC`, `CET`; when it is empty the offset is written
/// in its place, as `+0200`.
pub fn format_time(t: Time, zone: &str, layout: &str) -> String {
    let c = t.civil();
    let offset = t.offset_seconds();
    let local = t.unix() + i64::from(offset);
    let days = local.div_euclid(86_400);
    let mut yday = c.day;
    for m in 1..c.month {
        yday += crate::time::days_in_month(c.year, m);
    }
    let mut out = String::new();
    let mut layout = layout;
    while !layout.is_empty() {
        let (prefix, chunk, suffix) = next_chunk(layout);
        // The separator of a fraction is part of the prefix; it is written
        // only when digits follow it.
        let frac_sep = matches!(chunk, Some(Chunk::FracFixed(_) | Chunk::FracTrim(_)));
        if frac_sep {
            out.push_str(&prefix[..prefix.len() - 1]);
        } else {
            out.push_str(prefix);
        }
        let Some(chunk) = chunk else {
            break;
        };
        layout = suffix;
        match chunk {
            Chunk::Year => out.push_str(&format!("{:02}", c.year.rem_euclid(100))),
            Chunk::LongYear => out.push_str(&format!("{:04}", c.year)),
            Chunk::Month => out.push_str(&MONTHS[c.month as usize - 1][..3]),
            Chunk::LongMonth => out.push_str(MONTHS[c.month as usize - 1]),
            Chunk::NumMonth => out.push_str(&c.month.to_string()),
            Chunk::ZeroMonth => out.push_str(&format!("{:02}", c.month)),
            Chunk::WeekDay => out.push_str(&DAYS[weekday(days)][..3]),
            Chunk::LongWeekDay => out.push_str(DAYS[weekday(days)]),
            Chunk::Day => out.push_str(&c.day.to_string()),
            Chunk::UnderDay => out.push_str(&format!("{:2}", c.day)),
            Chunk::ZeroDay => out.push_str(&format!("{:02}", c.day)),
            Chunk::UnderYearDay => out.push_str(&format!("{yday:3}")),
            Chunk::ZeroYearDay => out.push_str(&format!("{yday:03}")),
            Chunk::Hour => out.push_str(&format!("{:02}", c.hour)),
            Chunk::Hour12 | Chunk::ZeroHour12 => {
                let h = match c.hour % 12 {
                    0 => 12,
                    h => h,
                };
                if chunk == Chunk::Hour12 {
                    out.push_str(&h.to_string());
                } else {
                    out.push_str(&format!("{h:02}"));
                }
            }
            Chunk::Minute => out.push_str(&c.minute.to_string()),
            Chunk::ZeroMinute => out.push_str(&format!("{:02}", c.minute)),
            Chunk::Second => out.push_str(&c.second.to_string()),
            Chunk::ZeroSecond => out.push_str(&format!("{:02}", c.second)),
            Chunk::Pm => out.push_str(if c.hour >= 12 { "PM" } else { "AM" }),
            Chunk::PmLower => out.push_str(if c.hour >= 12 { "pm" } else { "am" }),
            Chunk::Iso8601Tz
            | Chunk::Iso8601ColonTz
            | Chunk::Iso8601SecondsTz
            | Chunk::Iso8601ShortTz
            | Chunk::Iso8601ColonSecondsTz
                if offset == 0 =>
            {
                out.push('Z')
            }
            Chunk::Iso8601Tz | Chunk::NumTz => zone_offset(&mut out, offset, false, false, false),
            Chunk::Iso8601ColonTz | Chunk::NumColonTz => {
                zone_offset(&mut out, offset, true, false, false)
            }
            Chunk::Iso8601SecondsTz | Chunk::NumSecondsTz => {
                zone_offset(&mut out, offset, false, true, false)
            }
            Chunk::Iso8601ColonSecondsTz | Chunk::NumColonSecondsTz => {
                zone_offset(&mut out, offset, true, true, false)
            }
            Chunk::Iso8601ShortTz | Chunk::NumShortTz => {
                zone_offset(&mut out, offset, false, false, true)
            }
            Chunk::Tz => {
                if zone.is_empty() {
                    // No abbreviation: the offset in hours and minutes.
                    zone_offset(&mut out, offset, false, false, false);
                } else {
                    out.push_str(zone);
                }
            }
            Chunk::FracFixed(n) | Chunk::FracTrim(n) => {
                let sep = &prefix[prefix.len() - 1..];
                let digits = format!("{:09}", c.nanosecond);
                let mut frac = if n <= 9 {
                    digits[..n].to_string()
                } else {
                    format!("{digits}{}", "0".repeat(n - 9))
                };
                if matches!(chunk, Chunk::FracTrim(_)) {
                    frac = frac.trim_end_matches('0').to_string();
                }
                if !frac.is_empty() {
                    out.push_str(sep);
                    out.push_str(&frac);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        // 2026-03-31T10:30:05.120+02:00, a Tuesday.
        let t = Time::from_civil(2026, 3, 31, 10, 30, 5, 120_000_000, 7200);
        for (layout, want) in [
            (RFC3339, "2026-03-31T10:30:05+02:00"),
            (RFC1123, "Tue, 31 Mar 2026 10:30:05 test"),
            ("2006-01-02", "2026-03-31"),
            ("02 Jan 06 15:04 -0700", "31 Mar 26 10:30 +0200"),
            ("Monday, January 2 3:04PM", "Tuesday, March 31 10:30AM"),
            ("15:04:05.000", "10:30:05.120"),
            ("15:04:05.999", "10:30:05.12"),
            ("15:04:05.999999999Z07", "10:30:05.12+02"),
            ("_2/1 __2 002", "31/3  90 090"),
            ("no elements here", "no elements here"),
            ("x_2006", "x_2026"),
            ("03pm -07:00:00", "10am +02:00:00"),
        ] {
            assert_eq!(format_time(t, "test", layout), want, "{layout}");
        }
        let utc = t.utc();
        assert_eq!(format_time(utc, "UTC", RFC3339), "2026-03-31T08:30:05Z");
        assert_eq!(
            format_time(utc, "UTC", RFC1123),
            "Tue, 31 Mar 2026 08:30:05 UTC"
        );
        assert_eq!(format_time(t, "", "MST"), "+0200");
        let whole = Time::from_civil(1999, 12, 5, 23, 0, 0, 0, -3600);
        assert_eq!(
            format_time(whole, "", "Mon 2 Jan 2006 3PM.999 Z0700"),
            "Sun 5 Dec 1999 11PM -0100"
        );
    }
}
