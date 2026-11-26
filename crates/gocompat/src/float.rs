/// The shortest decimal digits that round-trip `f`, and the decimal
/// exponent of the first digit.
fn shortest(f: f64) -> (bool, String, i32) {
    let s = format!("{:e}", f.abs());
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    (f.is_sign_negative(), digits, exp.parse().unwrap_or(0))
}

fn special(f: f64) -> Option<String> {
    if f.is_nan() {
        Some("NaN".to_string())
    } else if f.is_infinite() {
        Some(if f > 0.0 { "+Inf" } else { "-Inf" }.to_string())
    } else {
        None
    }
}

fn fixed(neg: bool, digits: &str, exp: i32) -> String {
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if exp < 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat((-exp - 1) as usize));
        out.push_str(digits);
    } else {
        let int_len = exp as usize + 1;
        if digits.len() <= int_len {
            out.push_str(digits);
            out.push_str(&"0".repeat(int_len - digits.len()));
        } else {
            out.push_str(&digits[..int_len]);
            out.push('.');
            out.push_str(&digits[int_len..]);
        }
    }
    out
}

fn exponent(neg: bool, digits: &str, exp: i32, pad: bool) -> String {
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&digits[..1]);
    if digits.len() > 1 {
        out.push('.');
        out.push_str(&digits[1..]);
    }
    out.push('e');
    out.push(if exp < 0 { '-' } else { '+' });
    if pad {
        out.push_str(&format!("{:02}", exp.abs()));
    } else {
        out.push_str(&exp.abs().to_string());
    }
    out
}

/// Formats a number the way a `%v` of a 64-bit float reads: the shortest
/// digits that round-trip, as `1.5`, `100`, `1e+21` or `1e-05` (exponent
/// form below 1e-4 and from 1e21 up).
pub fn format_float_g(f: f64) -> String {
    if let Some(s) = special(f) {
        return s;
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let (neg, digits, exp) = shortest(f);
    if !(-4..21).contains(&exp) {
        exponent(neg, &digits, exp, true)
    } else {
        fixed(neg, &digits, exp)
    }
}

/// Formats a number as a JSON encoder writes it: fixed notation, except
/// below 1e-6 and from 1e21 up, where it is `1e-7` or `1e+21`.
pub fn format_float_json(f: f64) -> String {
    if let Some(s) = special(f) {
        return s;
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let (neg, digits, exp) = shortest(f);
    if !(-6..21).contains(&exp) {
        exponent(neg, &digits, exp, exp >= 0)
    } else {
        fixed(neg, &digits, exp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g() {
        assert_eq!(format_float_g(1.0), "1");
        assert_eq!(format_float_g(1.5), "1.5");
        assert_eq!(format_float_g(-0.25), "-0.25");
        assert_eq!(format_float_g(100000.0), "100000");
        assert_eq!(format_float_g(1e20), "100000000000000000000");
        assert_eq!(format_float_g(1e21), "1e+21");
        assert_eq!(format_float_g(0.0001), "0.0001");
        assert_eq!(format_float_g(0.00001), "1e-05");
        assert_eq!(format_float_g(1.25e-7), "1.25e-07");
        assert_eq!(format_float_g(0.0), "0");
    }

    #[test]
    fn json() {
        assert_eq!(format_float_json(3.0), "3");
        assert_eq!(format_float_json(0.000001), "0.000001");
        assert_eq!(format_float_json(0.0000001), "1e-7");
        assert_eq!(format_float_json(1e21), "1e+21");
        assert_eq!(format_float_json(12.5), "12.5");
    }
}
