//! Formatting a [`Time`] with a reference-time layout: the layout shows
//! how `Mon Jan 2 15:04:05 MST 2006` should look (`2006-01-02`,
//! `02 Jan 06 15:04`), and the time is written the same way. This is the
//! format `{{$datetime "2006-01-02"}}` takes, so the layouts in existing
//! request files keep their meaning.

use crate::time::{Civil, Time};

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

/// The layout elements that are a fixed text, each before any that is a
/// prefix of it: the first that matches is the element.
const ELEMENTS: [(&str, Chunk); 33] = [
    ("January", Chunk::LongMonth),
    ("Jan", Chunk::Month),
    ("Monday", Chunk::LongWeekDay),
    ("Mon", Chunk::WeekDay),
    ("MST", Chunk::Tz),
    ("01", Chunk::ZeroMonth),
    ("02", Chunk::ZeroDay),
    ("03", Chunk::ZeroHour12),
    ("04", Chunk::ZeroMinute),
    ("05", Chunk::ZeroSecond),
    ("06", Chunk::Year),
    ("002", Chunk::ZeroYearDay),
    ("15", Chunk::Hour),
    ("1", Chunk::NumMonth),
    ("2006", Chunk::LongYear),
    ("2", Chunk::Day),
    ("__2", Chunk::UnderYearDay),
    ("_2", Chunk::UnderDay),
    ("3", Chunk::Hour12),
    ("4", Chunk::Minute),
    ("5", Chunk::Second),
    ("PM", Chunk::Pm),
    ("pm", Chunk::PmLower),
    ("-070000", Chunk::NumSecondsTz),
    ("-07:00:00", Chunk::NumColonSecondsTz),
    ("-0700", Chunk::NumTz),
    ("-07:00", Chunk::NumColonTz),
    ("-07", Chunk::NumShortTz),
    ("Z070000", Chunk::Iso8601SecondsTz),
    ("Z07:00:00", Chunk::Iso8601ColonSecondsTz),
    ("Z0700", Chunk::Iso8601Tz),
    ("Z07:00", Chunk::Iso8601ColonTz),
    ("Z07", Chunk::Iso8601ShortTz),
];

/// Finds the next layout element in `layout`: the text before it, the
/// element, and the text after it.
fn next_chunk(layout: &str) -> (&str, Option<Chunk>, &str) {
    let found = |i: usize, len: usize, c: Chunk| (&layout[..i], Some(c), &layout[i + len..]);
    // Every element is ASCII, so only character boundaries can start one.
    for (i, _) in layout.char_indices() {
        let rest = &layout[i..];
        if rest.starts_with("_2006") {
            // `_2006` is a literal underscore then the year.
            return found(i + 1, 4, Chunk::LongYear);
        }
        if let Some(&(text, c)) = ELEMENTS.iter().find(|(text, _)| rest.starts_with(text)) {
            return found(i, text.len(), c);
        }
        if let Some((c, end)) = fraction_at(layout.as_bytes(), i) {
            return (&layout[..i + 1], Some(c), &layout[end..]);
        }
    }
    (layout, None, "")
}

/// A fraction of a second at `i`: a `.` or `,`, then a run of 0s or of 9s
/// that no other digit follows. Returns the element and where it ends.
fn fraction_at(b: &[u8], i: usize) -> Option<(Chunk, usize)> {
    if !matches!(b[i], b'.' | b',') {
        return None;
    }
    let digit = *b.get(i + 1).filter(|&&d| d == b'0' || d == b'9')?;
    let mut j = i + 1;
    while j < b.len() && b[j] == digit {
        j += 1;
    }
    if j < b.len() && b[j].is_ascii_digit() {
        return None;
    }
    let n = j - (i + 1);
    let c = if digit == b'0' {
        Chunk::FracFixed(n)
    } else {
        Chunk::FracTrim(n)
    };
    Some((c, j))
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

/// The time being written, broken into what the layout elements show.
struct Fields<'a> {
    civil: Civil,
    /// Days since the epoch, in the time's own offset.
    days: i64,
    /// The day of the year, from 1.
    yday: u32,
    offset: i32,
    zone: &'a str,
}

impl Fields<'_> {
    /// The text of an element that shows part of the date.
    fn date(&self, chunk: Chunk) -> Option<String> {
        let c = &self.civil;
        let yday = self.yday;
        Some(match chunk {
            Chunk::Year => format!("{:02}", c.year.rem_euclid(100)),
            Chunk::LongYear => format!("{:04}", c.year),
            Chunk::Month => MONTHS[c.month as usize - 1][..3].to_string(),
            Chunk::LongMonth => MONTHS[c.month as usize - 1].to_string(),
            Chunk::NumMonth => c.month.to_string(),
            Chunk::ZeroMonth => format!("{:02}", c.month),
            Chunk::WeekDay => DAYS[weekday(self.days)][..3].to_string(),
            Chunk::LongWeekDay => DAYS[weekday(self.days)].to_string(),
            Chunk::Day => c.day.to_string(),
            Chunk::UnderDay => format!("{:2}", c.day),
            Chunk::ZeroDay => format!("{:02}", c.day),
            Chunk::UnderYearDay => format!("{yday:3}"),
            Chunk::ZeroYearDay => format!("{yday:03}"),
            _ => return None,
        })
    }

    /// The text of an element that shows part of the time of day.
    fn clock(&self, chunk: Chunk) -> Option<String> {
        let c = &self.civil;
        let hour12 = match c.hour % 12 {
            0 => 12,
            h => h,
        };
        Some(match chunk {
            Chunk::Hour => format!("{:02}", c.hour),
            Chunk::Hour12 => hour12.to_string(),
            Chunk::ZeroHour12 => format!("{hour12:02}"),
            Chunk::Minute => c.minute.to_string(),
            Chunk::ZeroMinute => format!("{:02}", c.minute),
            Chunk::Second => c.second.to_string(),
            Chunk::ZeroSecond => format!("{:02}", c.second),
            Chunk::Pm => if c.hour >= 12 { "PM" } else { "AM" }.to_string(),
            Chunk::PmLower => if c.hour >= 12 { "pm" } else { "am" }.to_string(),
            _ => return None,
        })
    }

    /// Writes an element that shows the zone; any other writes nothing.
    fn write_zone(&self, out: &mut String, chunk: Chunk) {
        let offset = self.offset;
        match chunk {
            Chunk::Iso8601Tz
            | Chunk::Iso8601ColonTz
            | Chunk::Iso8601SecondsTz
            | Chunk::Iso8601ShortTz
            | Chunk::Iso8601ColonSecondsTz
                if offset == 0 =>
            {
                out.push('Z')
            }
            Chunk::Iso8601Tz | Chunk::NumTz => zone_offset(out, offset, false, false, false),
            Chunk::Iso8601ColonTz | Chunk::NumColonTz => {
                zone_offset(out, offset, true, false, false)
            }
            Chunk::Iso8601SecondsTz | Chunk::NumSecondsTz => {
                zone_offset(out, offset, false, true, false)
            }
            Chunk::Iso8601ColonSecondsTz | Chunk::NumColonSecondsTz => {
                zone_offset(out, offset, true, true, false)
            }
            Chunk::Iso8601ShortTz | Chunk::NumShortTz => {
                zone_offset(out, offset, false, false, true)
            }
            Chunk::Tz => {
                if self.zone.is_empty() {
                    // No abbreviation: the offset in hours and minutes.
                    zone_offset(out, offset, false, false, false);
                } else {
                    out.push_str(self.zone);
                }
            }
            _ => {}
        }
    }

    /// Writes a fraction of a second of `n` digits after its separator,
    /// or nothing when trimming leaves no digits.
    fn write_fraction(&self, out: &mut String, sep: &str, n: usize, trim: bool) {
        let digits = format!("{:09}", self.civil.nanosecond);
        let mut frac = if n <= 9 {
            digits[..n].to_string()
        } else {
            format!("{digits}{}", "0".repeat(n - 9))
        };
        if trim {
            frac = frac.trim_end_matches('0').to_string();
        }
        if !frac.is_empty() {
            out.push_str(sep);
            out.push_str(&frac);
        }
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
    let mut yday = c.day;
    for m in 1..c.month {
        yday += crate::time::days_in_month(c.year, m);
    }
    let fields = Fields {
        civil: c,
        days: local.div_euclid(86_400),
        yday,
        offset,
        zone,
    };
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
        if let Chunk::FracFixed(n) | Chunk::FracTrim(n) = chunk {
            let sep = &prefix[prefix.len() - 1..];
            fields.write_fraction(&mut out, sep, n, matches!(chunk, Chunk::FracTrim(_)));
        } else if let Some(text) = fields.date(chunk).or_else(|| fields.clock(chunk)) {
            out.push_str(&text);
        } else {
            fields.write_zone(&mut out, chunk);
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
            ("caf\u{e9} 2006 \u{2713}", "caf\u{e9} 2026 \u{2713}"),
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
