use std::time::Duration;

use apic_gocompat::{parse_duration, quote};

/// The wait between attempts when a retry policy gives none.
pub const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Parses a retry policy, `<attempts> [interval]`: how many times to send
/// a request until its assertions pass, and how long to wait between
/// attempts (a duration such as 500ms or 2s; default 1s). Attempts must be
/// at least 1. It is the value of `# @retry`, of `--retry` and of retry in
/// apic.yaml.
pub fn parse_retry(s: &str) -> Result<(usize, Duration), String> {
    let fields: Vec<&str> = s.split_whitespace().collect();
    if fields.is_empty() || fields.len() > 2 {
        return Err("want `<attempts> [interval]`, for example `10 2s`".to_string());
    }
    let n = match fields[0].parse::<i64>() {
        Ok(n) if n >= 1 => n as usize,
        _ => {
            return Err(format!(
                "attempts must be a whole number of at least 1, got {}",
                quote(fields[0])
            ))
        }
    };
    let mut interval = DEFAULT_RETRY_INTERVAL;
    if fields.len() == 2 {
        interval = parse_duration(fields[1]).map_err(|_| {
            format!(
                "interval must be a duration such as 500ms or 2s, got {}",
                quote(fields[1])
            )
        })?;
    }
    Ok((n, interval))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_retry_cases() {
        let good: [(&str, usize, Duration); 7] = [
            ("1", 1, Duration::from_secs(1)),
            ("10", 10, Duration::from_secs(1)),
            ("5 500ms", 5, Duration::from_millis(500)),
            (" 30  2s ", 30, Duration::from_secs(2)),
            ("3 0s", 3, Duration::ZERO),
            ("2 1m30s", 2, Duration::from_secs(90)),
            ("4\t250ms", 4, Duration::from_millis(250)),
        ];
        for (input, n, d) in good {
            assert_eq!(parse_retry(input), Ok((n, d)), "parse_retry({input:?})");
        }
        for input in ["", "0", "-1", "x", "1.5", "3 soon", "3 -1s", "3 2s extra"] {
            assert!(
                parse_retry(input).is_err(),
                "parse_retry({input:?}) should fail"
            );
        }
    }
}
