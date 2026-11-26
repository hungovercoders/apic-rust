//! An instant with the UTC offset it was written in, read from and written
//! to the RFC 3339 text apic's files hold (`2026-10-06T09:00:00.123Z`,
//! `2026-10-06T10:00:00+01:00`). Keeping the offset means a time read from
//! a file is written back exactly as it was.

use std::cmp::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// An instant to the nanosecond, with a UTC offset used only for display.
/// Two times are equal when they are the same instant, whatever their
/// offsets.
#[derive(Clone, Copy, Debug)]
pub struct Time {
    secs: i64,
    nanos: u32,
    offset: i32,
}

/// A time broken into calendar fields, in the time's own offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub nanosecond: u32,
}

impl PartialEq for Time {
    fn eq(&self, other: &Self) -> bool {
        self.secs == other.secs && self.nanos == other.nanos
    }
}

impl Eq for Time {}

impl PartialOrd for Time {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Time {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.secs, self.nanos).cmp(&(other.secs, other.nanos))
    }
}

impl std::hash::Hash for Time {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.secs.hash(state);
        self.nanos.hash(state);
    }
}

/// Days from 1970-01-01 to the given date of the proleptic Gregorian
/// calendar.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The number of days in a month of a year.
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 31,
    }
}

impl Time {
    /// The current time, in UTC.
    pub fn now() -> Time {
        Time::from_system_time(SystemTime::now())
    }

    /// The time `secs` seconds and `nanos` nanoseconds after 1970-01-01
    /// UTC, in UTC.
    pub fn from_unix(secs: i64, nanos: u32) -> Time {
        Time {
            secs: secs + i64::from(nanos / 1_000_000_000),
            nanos: nanos % 1_000_000_000,
            offset: 0,
        }
    }

    pub fn from_system_time(t: SystemTime) -> Time {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Time::from_unix(d.as_secs() as i64, d.subsec_nanos()),
            Err(e) => {
                let d = e.duration();
                let mut secs = -(d.as_secs() as i64);
                let mut nanos = 0;
                if d.subsec_nanos() > 0 {
                    secs -= 1;
                    nanos = 1_000_000_000 - d.subsec_nanos();
                }
                Time::from_unix(secs, nanos)
            }
        }
    }

    pub fn to_system_time(self) -> SystemTime {
        if self.secs >= 0 {
            UNIX_EPOCH + Duration::new(self.secs as u64, self.nanos)
        } else {
            UNIX_EPOCH - Duration::from_secs(self.secs.unsigned_abs())
                + Duration::from_nanos(u64::from(self.nanos))
        }
    }

    /// The time of the calendar fields given, which are read in a zone
    /// `offset` seconds east of UTC. The fields are not range-checked.
    #[allow(clippy::too_many_arguments)]
    pub fn from_civil(
        year: i64,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        nanosecond: u32,
        offset: i32,
    ) -> Time {
        let days = days_from_civil(year, month, day);
        let secs =
            days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second)
                - i64::from(offset);
        Time {
            secs,
            nanos: nanosecond,
            offset,
        }
    }

    /// Seconds since 1970-01-01 UTC.
    pub fn unix(self) -> i64 {
        self.secs
    }

    /// The nanoseconds within the second.
    pub fn nanosecond(self) -> u32 {
        self.nanos
    }

    /// Nanoseconds since 1970-01-01 UTC.
    pub fn unix_nanos(self) -> i128 {
        i128::from(self.secs) * 1_000_000_000 + i128::from(self.nanos)
    }

    /// The offset the time is displayed in, in seconds east of UTC.
    pub fn offset_seconds(self) -> i32 {
        self.offset
    }

    /// The same instant, displayed in UTC.
    pub fn utc(self) -> Time {
        Time { offset: 0, ..self }
    }

    /// The same instant, displayed `offset` seconds east of UTC.
    pub fn with_offset(self, offset: i32) -> Time {
        Time { offset, ..self }
    }

    /// The calendar fields, in the time's own offset.
    pub fn civil(self) -> Civil {
        let local = self.secs + i64::from(self.offset);
        let days = local.div_euclid(86_400);
        let rem = local.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        Civil {
            year,
            month,
            day,
            hour: rem / 3600,
            minute: rem % 3600 / 60,
            second: rem % 60,
            nanosecond: self.nanos,
        }
    }

    /// The time `nanos` nanoseconds later (earlier when negative).
    pub fn add_nanos(self, nanos: i128) -> Time {
        let total = self.unix_nanos() + nanos;
        Time {
            secs: total.div_euclid(1_000_000_000) as i64,
            nanos: total.rem_euclid(1_000_000_000) as u32,
            offset: self.offset,
        }
    }

    /// Nanoseconds from `earlier` to this time; negative when this time is
    /// the earlier one.
    pub fn nanos_since(self, earlier: Time) -> i128 {
        self.unix_nanos() - earlier.unix_nanos()
    }

    /// Whether this time is later than `other`.
    pub fn after(self, other: Time) -> bool {
        self > other
    }

    /// Whether this time is earlier than `other`.
    pub fn before(self, other: Time) -> bool {
        self < other
    }

    /// Reads an RFC 3339 time: `2006-01-02T15:04:05Z`, with an optional
    /// fraction of a second and either `Z` or a `+07:00` offset.
    pub fn parse_rfc3339(s: &str) -> Result<Time, String> {
        let bad = || format!("parsing time {s:?} as \"2006-01-02T15:04:05Z07:00\"");
        let b = s.as_bytes();
        let num = |from: usize, len: usize| -> Option<u32> {
            let part = b.get(from..from + len)?;
            if !part.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(part).ok()?.parse().ok()
        };
        let lit = |at: usize, c: u8| b.get(at) == Some(&c);
        if !(lit(4, b'-') && lit(7, b'-') && lit(10, b'T') && lit(13, b':') && lit(16, b':')) {
            return Err(bad());
        }
        let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
            num(0, 4),
            num(5, 2),
            num(8, 2),
            num(11, 2),
            num(14, 2),
            num(17, 2),
        ) else {
            return Err(bad());
        };
        let year = i64::from(year);
        if !(1..=12).contains(&month)
            || day < 1
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(format!("{}: out of range", bad()));
        }
        let mut i = 19;
        let mut nanos = 0u32;
        if lit(i, b'.') || lit(i, b',') {
            let start = i + 1;
            let mut end = start;
            while b.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == start {
                return Err(bad());
            }
            let mut scale = 100_000_000u32;
            for &d in &b[start..end.min(start + 9)] {
                nanos += u32::from(d - b'0') * scale;
                scale /= 10;
            }
            i = end;
        }
        let offset = match b.get(i) {
            Some(b'Z') if i + 1 == b.len() => 0,
            Some(&sign @ (b'+' | b'-')) if i + 6 == b.len() && lit(i + 3, b':') => {
                let (Some(h), Some(m)) = (num(i + 1, 2), num(i + 4, 2)) else {
                    return Err(bad());
                };
                if h > 23 || m > 59 {
                    return Err(format!("{}: out of range", bad()));
                }
                let secs = (h * 3600 + m * 60) as i32;
                if sign == b'-' {
                    -secs
                } else {
                    secs
                }
            }
            _ => return Err(bad()),
        };
        Ok(Time::from_civil(
            year, month, day, hour, minute, second, nanos, offset,
        ))
    }

    /// Writes the time as RFC 3339 in its own offset, with as many digits
    /// of the fraction of a second as it needs and none when it is whole:
    /// `2026-10-06T09:00:00.123Z`, `2026-10-06T10:00:00+01:00`.
    pub fn format_rfc3339_nano(self) -> String {
        let c = self.civil();
        let mut out = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            c.year, c.month, c.day, c.hour, c.minute, c.second
        );
        if c.nanosecond != 0 {
            let frac = format!("{:09}", c.nanosecond);
            out.push('.');
            out.push_str(frac.trim_end_matches('0'));
        }
        if self.offset == 0 {
            out.push('Z');
        } else {
            let minutes = self.offset.abs() / 60;
            out.push(if self.offset < 0 { '-' } else { '+' });
            out.push_str(&format!("{:02}:{:02}", minutes / 60, minutes % 60));
        }
        out
    }
}

impl std::ops::Add<Duration> for Time {
    type Output = Time;

    fn add(self, d: Duration) -> Time {
        self.add_nanos(d.as_nanos() as i128)
    }
}

impl std::ops::Sub<Duration> for Time {
    type Output = Time;

    fn sub(self, d: Duration) -> Time {
        self.add_nanos(-(d.as_nanos() as i128))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for s in [
            "2026-10-06T09:00:00Z",
            "2026-10-06T08:00:00.123Z",
            "2026-10-06T10:00:00.000000001+01:00",
            "1999-12-31T23:59:59.5-07:30",
            "2024-02-29T00:00:00Z",
            "0001-01-01T00:00:00Z",
        ] {
            let t = Time::parse_rfc3339(s).unwrap();
            assert_eq!(t.format_rfc3339_nano(), s);
        }
        let a = Time::parse_rfc3339("2026-10-06T10:00:00+01:00").unwrap();
        let b = Time::parse_rfc3339("2026-10-06T09:00:00Z").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.utc().format_rfc3339_nano(), "2026-10-06T09:00:00Z");
        assert_eq!(b.unix(), 1_791_277_200);
        assert_eq!(
            Time::from_unix(0, 0).format_rfc3339_nano(),
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(
            Time::from_unix(-1, 500_000_000).format_rfc3339_nano(),
            "1969-12-31T23:59:59.5Z"
        );
    }

    #[test]
    fn rejects_what_is_not_rfc3339() {
        for s in [
            "",
            "2026-10-06",
            "2026-10-06 09:00:00Z",
            "2026-13-06T09:00:00Z",
            "2026-02-30T09:00:00Z",
            "2026-10-06T24:00:00Z",
            "2026-10-06T09:00:00",
            "2026-10-06T09:00:00.Z",
            "2026-10-06T09:00:00+0100",
            "2026-10-06T09:00:00Zx",
        ] {
            assert!(Time::parse_rfc3339(s).is_err(), "{s}");
        }
    }

    #[test]
    fn arithmetic() {
        let t = Time::parse_rfc3339("2026-10-06T09:00:00Z").unwrap();
        let later = t + Duration::from_secs(3600);
        assert_eq!(later.format_rfc3339_nano(), "2026-10-06T10:00:00Z");
        assert!(later.after(t) && t.before(later));
        assert_eq!(later.nanos_since(t), 3_600_000_000_000);
        assert_eq!(later - Duration::from_secs(3600), t);
        assert_eq!(Time::from_system_time(t.to_system_time()), t);
    }
}
