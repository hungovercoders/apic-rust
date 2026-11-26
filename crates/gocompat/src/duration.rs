use std::time::Duration;

/// Formats a duration as `72h3m0.5s`, `1.5ms`, `100µs` or `0s`: hours,
/// minutes and seconds above a second, a single fractional unit below it,
/// with trailing zeros removed.
pub fn format_duration(d: Duration) -> String {
    let ns = d.as_nanos();
    if ns == 0 {
        return "0s".to_string();
    }
    if ns < 1_000_000_000 {
        let (unit, div) = if ns < 1_000 {
            return format!("{ns}ns");
        } else if ns < 1_000_000 {
            ("µs", 1_000u128)
        } else {
            ("ms", 1_000_000u128)
        };
        return format!("{}{}", frac(ns, div), unit);
    }
    let total_secs = ns / 1_000_000_000;
    let rem_ns = ns % 1_000_000_000;
    let h = total_secs / 3600;
    let m = (total_secs / 60) % 60;
    let s = total_secs % 60;
    let secs = frac(s * 1_000_000_000 + rem_ns, 1_000_000_000);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if h > 0 || m > 0 {
        out.push_str(&format!("{m}m"));
    }
    out.push_str(&secs);
    out.push('s');
    out
}

/// `value / div` as a decimal with no trailing zeros.
fn frac(value: u128, div: u128) -> String {
    let whole = value / div;
    let rem = value % div;
    if rem == 0 {
        return whole.to_string();
    }
    let width = div.to_string().len() - 1;
    let digits = format!("{rem:0width$}");
    format!("{}.{}", whole, digits.trim_end_matches('0'))
}

/// Parses `300ms`, `1.5s`, `2m30s`, `1h` and the like: one or more decimal
/// numbers, each with a unit of `ns`, `us`, `µs`, `ms`, `s`, `m` or `h`.
/// A bare `0` is allowed; negative durations are not.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let orig = s;
    let bad = || format!("time: invalid duration {}", crate::quote(orig));
    let mut s = s.strip_prefix('+').unwrap_or(s);
    if s.starts_with('-') {
        return Err(bad());
    }
    if s == "0" {
        return Ok(Duration::ZERO);
    }
    if s.is_empty() {
        return Err(bad());
    }
    let mut total: f64 = 0.0;
    while !s.is_empty() {
        let num_len = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(s.len());
        let num = &s[..num_len];
        if num.is_empty() || num == "." || num.matches('.').count() > 1 {
            return Err(bad());
        }
        let value: f64 = num.parse().map_err(|_| bad())?;
        s = &s[num_len..];
        let unit_len = s
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(s.len());
        let unit = &s[..unit_len];
        if unit.is_empty() {
            return Err(format!(
                "time: missing unit in duration {}",
                crate::quote(orig)
            ));
        }
        let scale = match unit {
            "ns" => 1.0,
            "us" | "µs" | "μs" => 1e3,
            "ms" => 1e6,
            "s" => 1e9,
            "m" => 60e9,
            "h" => 3600e9,
            _ => {
                return Err(format!(
                    "time: unknown unit {} in duration {}",
                    crate::quote(unit),
                    crate::quote(orig)
                ))
            }
        };
        total += value * scale;
        s = &s[unit_len..];
    }
    if total > (i64::MAX as f64) {
        return Err(bad());
    }
    Ok(Duration::from_nanos(total.round() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        let cases: &[(u64, &str)] = &[
            (0, "0s"),
            (1, "1ns"),
            (1_500, "1.5µs"),
            (100_000, "100µs"),
            (1_500_000, "1.5ms"),
            (30_000_000_000, "30s"),
            (1_500_000_000, "1.5s"),
            (120_000_000_000, "2m0s"),
            (3_723_500_000_000, "1h2m3.5s"),
            (3_600_000_000_000, "1h0m0s"),
        ];
        for (ns, want) in cases {
            assert_eq!(format_duration(Duration::from_nanos(*ns)), *want);
        }
    }

    #[test]
    fn parses() {
        assert_eq!(parse_duration("300ms").unwrap(), Duration::from_millis(300));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("2m30s").unwrap(), Duration::from_secs(150));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("0").unwrap(), Duration::ZERO);
        assert!(parse_duration("").is_err());
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("5 s").is_err());
        assert!(parse_duration("-1s").is_err());
        assert!(parse_duration("1x").is_err());
    }
}
