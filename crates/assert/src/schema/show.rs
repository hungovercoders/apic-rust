//! How values read in a schema failure: `map[a:1 b:[x y]]` for an object,
//! `[1 2]` for an array, a string as it is. The failures were first worded
//! with these forms, and a user's saved expectations may quote them.

use apic_gocompat::{format_float_g, quote};
use serde_json::Value;

/// A value as it reads in most messages. None is a value that is not
/// there at all, which is how a JSON null instance reads.
pub(crate) fn display(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "<invalid reflect.Value>".to_string(),
        Some(v) => inner(v, false),
    }
}

/// A value where the message asks for text: anything that is not a string
/// says what it is, as `%!s(float64=1)`.
pub(crate) fn display_text(v: &Value) -> String {
    inner(v, true)
}

/// A list of values, as an `enum` reads.
pub(crate) fn display_list(vs: &[Value]) -> String {
    let parts: Vec<String> = vs.iter().map(|v| inner(v, false)).collect();
    format!("[{}]", parts.join(" "))
}

/// A value inside a list or on its own, where null reads `<nil>`.
pub(crate) fn display_value(v: &Value) -> String {
    inner(v, false)
}

fn inner(v: &Value, text: bool) -> String {
    match v {
        Value::Null => "<nil>".to_string(),
        Value::Bool(b) => {
            if text {
                format!("%!s(bool={b})")
            } else {
                b.to_string()
            }
        }
        Value::Number(n) => {
            let f = format_float_g(n.as_f64().unwrap_or(0.0));
            if text {
                format!("%!s(float64={f})")
            } else {
                f
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(|v| inner(v, text)).collect();
            format!("[{}]", parts.join(" "))
        }
        Value::Object(o) => {
            let parts: Vec<String> = o
                .iter()
                .map(|(k, v)| format!("{}:{}", k, inner(v, text)))
                .collect();
            format!("map[{}]", parts.join(" "))
        }
    }
}

/// A list of names, each quoted: `["a" "b"]`.
pub(crate) fn quoted_list(names: &[String]) -> String {
    let parts: Vec<String> = names.iter().map(|n| quote(n)).collect();
    format!("[{}]", parts.join(" "))
}

/// A float as an exact fraction in lowest terms, `3/2` or `7/1`.
pub(crate) fn rat_string(f: f64) -> String {
    if f == 0.0 || !f.is_finite() {
        return "0/1".to_string();
    }
    let bits = f.to_bits();
    let neg = bits >> 63 == 1;
    let exp_bits = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    // f = mantissa × 2^exp
    let (mut mantissa, mut exp) = if exp_bits == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), exp_bits - 1075)
    };
    while mantissa % 2 == 0 && exp < 0 {
        mantissa /= 2;
        exp += 1;
    }
    let sign = if neg { "-" } else { "" };
    if exp >= 0 {
        format!("{sign}{}/1", times_pow2(mantissa, exp as u32))
    } else {
        format!("{sign}{mantissa}/{}", times_pow2(1, (-exp) as u32))
    }
}

/// `n` × 2^`shift`, in decimal.
fn times_pow2(n: u64, shift: u32) -> String {
    // Decimal digits, least significant first.
    let mut digits: Vec<u8> = n.to_string().bytes().rev().map(|d| d - b'0').collect();
    for _ in 0..shift {
        let mut carry = 0;
        for d in &mut digits {
            let v = *d * 2 + carry;
            *d = v % 10;
            carry = v / 10;
        }
        if carry > 0 {
            digits.push(carry);
        }
    }
    digits.iter().rev().map(|d| char::from(d + b'0')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_read_as_they_did() {
        let v = json!({"b": [1, "x", null, true, 1.5], "a": {"k": 1e21}});
        assert_eq!(
            display(Some(&v)),
            "map[a:map[k:1e+21] b:[1 x <nil> true 1.5]]"
        );
        assert_eq!(display(Some(&Value::Null)), "<invalid reflect.Value>");
        assert_eq!(
            display_text(&json!([1, "a", true, null])),
            "[%!s(float64=1) a %!s(bool=true) <nil>]"
        );
        assert_eq!(
            display_list(&[json!("a"), json!(2), Value::Null]),
            "[a 2 <nil>]"
        );
        assert_eq!(
            quoted_list(&["a".to_string(), "b c".to_string()]),
            "[\"a\" \"b c\"]"
        );
    }

    #[test]
    fn fractions() {
        assert_eq!(rat_string(1.5), "3/2");
        assert_eq!(rat_string(7.0), "7/1");
        assert_eq!(rat_string(-0.25), "-1/4");
        assert_eq!(rat_string(0.0), "0/1");
        assert_eq!(rat_string(1e21), "1000000000000000000000/1");
        assert_eq!(rat_string(0.1), "3602879701896397/36028797018963968");
    }
}
