//! The machine's time zone, for `{{$localDatetime}}`: the offset and the
//! abbreviation in force at an instant, read from the system's zone file.

use std::sync::OnceLock;

/// `t` in the machine's time zone: the same instant, with the offset in
/// force there at that instant.
pub fn local_time(t: apic_gocompat::Time) -> apic_gocompat::Time {
    let (offset, _) = LocalZone::system().lookup(t.unix());
    t.with_offset(offset)
}

/// A time zone's rules.
#[derive(Debug, Default)]
pub(crate) struct LocalZone {
    /// When each period starts (seconds since 1970 UTC) and its type.
    transitions: Vec<(i64, usize)>,
    types: Vec<(i32, String)>,
    /// The rule for instants after the last transition.
    rule: Option<Rule>,
}

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    std: (i32, String),
    dst: Option<(i32, String, Change, Change)>,
}

/// When daylight time starts or ends: a day of the year and a time of day.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Change {
    day: Day,
    /// Seconds after local midnight.
    time: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Day {
    /// `Jn`: day 1 to 365, 29 February never counted.
    Julian(i64),
    /// `n`: day 0 to 365, 29 February counted.
    ZeroBased(i64),
    /// `Mm.w.d`: weekday `d` of week `w` (5 is the last) of month `m`.
    Month(i64, i64, i64),
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_before_year(y: i64) -> i64 {
    let y = y - 1;
    365 * (y - 1969) + (y / 4 - 1969 / 4) - (y / 100 - 1969 / 100) + (y / 400 - 1969 / 400)
}

impl Change {
    /// Seconds from the start of `year` (local) to the change.
    fn seconds_into(&self, year: i64) -> i64 {
        const BEFORE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let leap = is_leap(year);
        let day = match self.day {
            Day::Julian(n) => {
                if leap && n >= 60 {
                    n
                } else {
                    n - 1
                }
            }
            Day::ZeroBased(n) => n,
            Day::Month(m, w, d) => {
                let mut first = BEFORE[(m - 1) as usize];
                if leap && m > 2 {
                    first += 1;
                }
                // 1 January 1970 was a Thursday.
                let first_dow = (days_before_year(year) + first + 4).rem_euclid(7);
                let mut day = first + (d - first_dow).rem_euclid(7) + (w - 1) * 7;
                let mut len = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(m - 1) as usize];
                if leap && m == 2 {
                    len += 1;
                }
                if day >= first + len {
                    day -= 7;
                }
                day
            }
        };
        day * 86_400 + self.time
    }
}

impl Rule {
    /// Parses a POSIX TZ string: `CET-1CEST,M3.5.0,M10.5.0/3`.
    fn parse(s: &str) -> Option<Rule> {
        let mut p = s;
        let std_name = name(&mut p)?;
        let std_off = -offset(&mut p)?;
        if p.is_empty() {
            return Some(Rule {
                std: (std_off as i32, std_name),
                dst: None,
            });
        }
        let dst_name = name(&mut p)?;
        let dst_off = if p.is_empty() || p.starts_with(',') {
            std_off + 3600
        } else {
            -offset(&mut p)?
        };
        let (start, end) = if p.is_empty() {
            (
                Change {
                    day: Day::Month(3, 2, 0),
                    time: 7200,
                },
                Change {
                    day: Day::Month(11, 1, 0),
                    time: 7200,
                },
            )
        } else {
            p = p.strip_prefix(',')?;
            let start = change(&mut p)?;
            p = p.strip_prefix(',')?;
            let end = change(&mut p)?;
            (start, end)
        };
        Some(Rule {
            std: (std_off as i32, std_name),
            dst: Some((dst_off as i32, dst_name, start, end)),
        })
    }

    fn lookup(&self, unix: i64) -> (i32, String) {
        let Some((dst_off, dst_name, start, end)) = &self.dst else {
            return self.std.clone();
        };
        // The year of the instant, near enough at its edges.
        let mut year = 1970 + unix / 31_556_952;
        while days_before_year(year + 1) * 86_400 <= unix {
            year += 1;
        }
        while days_before_year(year) * 86_400 > unix {
            year -= 1;
        }
        let into = unix - days_before_year(year) * 86_400;
        let start_at = start.seconds_into(year) - i64::from(self.std.0);
        let end_at = end.seconds_into(year) - i64::from(*dst_off);
        let in_dst = if start_at < end_at {
            (start_at..end_at).contains(&into)
        } else {
            !(end_at..start_at).contains(&into)
        };
        if in_dst {
            (*dst_off, dst_name.clone())
        } else {
            self.std.clone()
        }
    }
}

fn name(p: &mut &str) -> Option<String> {
    if let Some(rest) = p.strip_prefix('<') {
        let end = rest.find('>')?;
        *p = &rest[end + 1..];
        return Some(rest[..end].to_string());
    }
    let end = p
        .find(|c: char| c.is_ascii_digit() || matches!(c, ',' | '-' | '+'))
        .unwrap_or(p.len());
    if end < 3 {
        return None;
    }
    let n = p[..end].to_string();
    *p = &p[end..];
    Some(n)
}

/// `[+-]hh[:mm[:ss]]` in seconds.
fn offset(p: &mut &str) -> Option<i64> {
    let neg = p.starts_with('-');
    if neg || p.starts_with('+') {
        *p = &p[1..];
    }
    let mut total = 0;
    for (n, unit) in [3600, 60, 1].into_iter().enumerate() {
        if n > 0 {
            match p.strip_prefix(':') {
                Some(rest) => *p = rest,
                None => break,
            }
        }
        let end = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
        total += p[..end].parse::<i64>().ok()? * unit;
        *p = &p[end..];
    }
    Some(if neg { -total } else { total })
}

fn number(p: &mut &str) -> Option<i64> {
    let end = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
    let n = p[..end].parse().ok()?;
    *p = &p[end..];
    Some(n)
}

fn change(p: &mut &str) -> Option<Change> {
    let day = if let Some(rest) = p.strip_prefix('J') {
        *p = rest;
        Day::Julian(number(p)?)
    } else if let Some(rest) = p.strip_prefix('M') {
        *p = rest;
        let m = number(p)?;
        *p = p.strip_prefix('.')?;
        let w = number(p)?;
        *p = p.strip_prefix('.')?;
        let d = number(p)?;
        if !(1..=12).contains(&m) || !(1..=5).contains(&w) || !(0..=6).contains(&d) {
            return None;
        }
        Day::Month(m, w, d)
    } else {
        Day::ZeroBased(number(p)?)
    };
    let time = match p.strip_prefix('/') {
        Some(rest) => {
            *p = rest;
            offset(p)?
        }
        None => 7200,
    };
    Some(Change { day, time })
}

fn be(b: &[u8]) -> i64 {
    match b.len() {
        4 => i64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]])),
        _ => i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
    }
}

impl LocalZone {
    /// Reads a zone file in the TZif format.
    fn parse(data: &[u8]) -> Option<LocalZone> {
        let header = |d: &[u8]| -> Option<(u8, [usize; 6])> {
            if d.len() < 44 || &d[..4] != b"TZif" {
                return None;
            }
            let mut counts = [0usize; 6];
            for (i, c) in counts.iter_mut().enumerate() {
                *c = be(&d[20 + 4 * i..24 + 4 * i]) as usize;
            }
            Some((d[4], counts))
        };
        let (version, c) = header(data)?;
        let block = |c: &[usize; 6], width: usize| {
            c[3] * width + c[3] + c[4] * 6 + c[5] + c[2] * (width + 4) + c[1] + c[0]
        };
        let (data, c, width) = if version >= b'2' {
            let rest = data.get(44 + block(&c, 4)..)?;
            let (_, c2) = header(rest)?;
            (rest, c2, 8)
        } else {
            (data, c, 4)
        };
        let [_, _, _, timecnt, typecnt, charcnt] = c;
        let body = data.get(44..44 + block(&c, width))?;
        let times = &body[..timecnt * width];
        let idx = &body[timecnt * width..timecnt * width + timecnt];
        let types_raw = &body[timecnt * (width + 1)..timecnt * (width + 1) + typecnt * 6];
        let chars = &body[timecnt * (width + 1) + typecnt * 6..][..charcnt];
        let mut types = Vec::new();
        for t in types_raw.chunks(6) {
            let abbr_at = t[5] as usize;
            let abbr = chars.get(abbr_at..)?;
            let end = abbr.iter().position(|&b| b == 0).unwrap_or(abbr.len());
            types.push((
                be(&t[..4]) as i32,
                String::from_utf8_lossy(&abbr[..end]).into_owned(),
            ));
        }
        let mut transitions = Vec::new();
        for (i, t) in times.chunks(width).enumerate() {
            let ty = idx[i] as usize;
            if ty >= types.len() {
                return None;
            }
            transitions.push((be(t), ty));
        }
        let rule = if width == 8 {
            let footer = data.get(44 + block(&c, 8)..)?;
            let text = String::from_utf8_lossy(footer);
            text.trim().lines().next().and_then(Rule::parse)
        } else {
            None
        };
        if types.is_empty() && rule.is_none() {
            return None;
        }
        Some(LocalZone {
            transitions,
            types,
            rule,
        })
    }

    fn load() -> LocalZone {
        let path = match std::env::var("TZ") {
            Err(_) => "/etc/localtime".to_string(),
            Ok(tz) => {
                let tz = tz.strip_prefix(':').unwrap_or(&tz);
                if tz.is_empty() || tz == "UTC" {
                    return LocalZone::default();
                }
                if tz.starts_with('/') {
                    tz.to_string()
                } else {
                    format!("/usr/share/zoneinfo/{tz}")
                }
            }
        };
        std::fs::read(path)
            .ok()
            .and_then(|d| LocalZone::parse(&d))
            .unwrap_or_default()
    }

    /// The machine's zone, read once; UTC when it cannot be read.
    pub fn system() -> &'static LocalZone {
        static ZONE: OnceLock<LocalZone> = OnceLock::new();
        ZONE.get_or_init(LocalZone::load)
    }

    /// The offset east of UTC, in seconds, and the abbreviation in force
    /// at an instant.
    pub fn lookup(&self, unix: i64) -> (i32, String) {
        let utc = || (0, "UTC".to_string());
        let after_last = self.transitions.last().is_none_or(|(at, _)| unix >= *at);
        if after_last {
            if let Some(rule) = &self.rule {
                return rule.lookup(unix);
            }
        }
        let n = self.transitions.partition_point(|(at, _)| *at <= unix);
        if n == 0 {
            return self.types.first().cloned().unwrap_or_else(utc);
        }
        self.types
            .get(self.transitions[n - 1].1)
            .cloned()
            .unwrap_or_else(utc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_rules() {
        let r = Rule::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        // 2026-01-15 and 2026-07-15, noon UTC.
        assert_eq!(r.lookup(1_768_478_400), (3600, "CET".to_string()));
        assert_eq!(r.lookup(1_784_116_800), (7200, "CEST".to_string()));
        // The last Sunday of March 2026 is the 29th: 01:00 UTC is the change.
        assert_eq!(r.lookup(1_774_746_000 - 1).0, 3600);
        assert_eq!(r.lookup(1_774_746_000).0, 7200);
        let r = Rule::parse("<+0530>-5:30").unwrap();
        assert_eq!(r.lookup(0), (19_800, "+0530".to_string()));
        // Southern hemisphere: daylight time spans the new year.
        let r = Rule::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(r.lookup(1_768_478_400).1, "AEDT");
        assert_eq!(r.lookup(1_784_116_800).1, "AEST");
        assert!(Rule::parse("nonsense").is_none());
    }

    #[test]
    fn reads_zone_files() {
        assert_eq!(LocalZone::default().lookup(0), (0, "UTC".to_string()));
        if let Ok(data) = std::fs::read("/usr/share/zoneinfo/Europe/Paris") {
            let z = LocalZone::parse(&data).expect("a zone file");
            assert_eq!(z.lookup(1_768_478_400), (3600, "CET".to_string()));
            assert_eq!(z.lookup(1_784_116_800), (7200, "CEST".to_string()));
            assert_eq!(z.lookup(946_728_000), (3600, "CET".to_string()));
        }
    }
}
