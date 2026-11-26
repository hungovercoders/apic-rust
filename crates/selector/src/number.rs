//! Exact decimal numbers, so that 9007199254740993 keeps its value instead
//! of rounding through a 64-bit float, and 1e1000 still compares as a
//! number.

use std::cmp::Ordering;

/// An exact number: every decimal literal, and the hexadecimal, octal and
/// binary ones a filter may be written with, has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Number {
    neg: bool,
    /// Decimal digits without leading or trailing zeros; empty for zero.
    digits: Vec<u8>,
    /// The value is `digits` × 10^`exp`.
    exp: i64,
}

/// The largest decimal exponent a literal may reach, either way.
const MAX_EXP5: i64 = 1_000_000;
/// The largest binary exponent a literal may reach, either way.
const MAX_EXP2: i64 = 10_000_000;
/// The largest power of two or five that is multiplied out digit by digit;
/// only a hexadecimal, octal or binary literal, or a `p` exponent, needs
/// one.
const MAX_RESIDUAL: i64 = 100_000;

/// A natural number in base 10^9, least significant limb first.
struct Natural(Vec<u32>);

impl Natural {
    const BASE: u64 = 1_000_000_000;

    fn mul_add(&mut self, mul: u32, add: u32) {
        let mut carry = u64::from(add);
        for limb in &mut self.0 {
            let v = u64::from(*limb) * u64::from(mul) + carry;
            *limb = (v % Self::BASE) as u32;
            carry = v / Self::BASE;
        }
        while carry > 0 {
            self.0.push((carry % Self::BASE) as u32);
            carry /= Self::BASE;
        }
    }

    fn mul_pow(&mut self, base: u32, mut n: i64) {
        // The largest power of the base that fits a 32-bit multiplier.
        let (step, chunk) = if base == 2 {
            (30, 1u32 << 30)
        } else {
            (13, 1_220_703_125)
        };
        while n >= step {
            self.mul_add(chunk, 0);
            n -= step;
        }
        for _ in 0..n {
            self.mul_add(base, 0);
        }
    }

    fn decimal(&self) -> Vec<u8> {
        let mut out = String::new();
        for (i, limb) in self.0.iter().rev().enumerate() {
            if i == 0 {
                out.push_str(&limb.to_string());
            } else {
                out.push_str(&format!("{limb:09}"));
            }
        }
        out.into_bytes()
    }
}

/// Reads the digits of an exponent: an optional sign, then digits that
/// may be separated by single underscores.
fn scan_exponent(b: &[u8]) -> Option<i64> {
    let mut i = 0;
    let mut text = String::new();
    if let Some(&c) = b.first() {
        if c == b'+' || c == b'-' {
            if c == b'-' {
                text.push('-');
            }
            i = 1;
        }
    }
    let mut prev = b'.';
    let mut has_digits = false;
    for &c in &b[i..] {
        if c.is_ascii_digit() {
            text.push(char::from(c));
            prev = b'0';
            has_digits = true;
        } else if c == b'_' {
            if prev != b'0' {
                return None;
            }
            prev = b'_';
        } else {
            // Whatever is left does not belong to the number.
            return None;
        }
    }
    if !has_digits || prev == b'_' {
        return None;
    }
    text.parse().ok()
}

/// The mantissa of a number as it is written.
struct Mantissa {
    base: u32,
    /// The value of each digit.
    digits: Vec<u8>,
    /// Minus the number of digits after the point.
    fcount: i64,
    /// Where the mantissa ends in the text.
    end: usize,
}

/// Reads a mantissa from `b[i..]`: a base prefix, then digits with an
/// optional point and single underscores between them.
fn scan_mantissa(b: &[u8], mut i: usize) -> Option<Mantissa> {
    let mut base = 10u32;
    let mut prev = b'.';
    let mut inval_sep = false;
    let mut count = 0i64;
    if b.get(i) == Some(&b'0') {
        prev = b'0';
        count = 1;
        i += 1;
        let prefixed = match b.get(i) {
            Some(b'b' | b'B') => Some(2),
            Some(b'o' | b'O') => Some(8),
            Some(b'x' | b'X') => Some(16),
            _ => None,
        };
        if let Some(n) = prefixed {
            base = n;
            count = 0;
            i += 1;
        }
    }
    let mut frac_ok = true;
    let mut dp: Option<i64> = None;
    let mut mantissa: Vec<u8> = Vec::new(); // digit values
    while let Some(&c) = b.get(i) {
        if c == b'.' && frac_ok {
            frac_ok = false;
            if prev == b'_' {
                inval_sep = true;
            }
            prev = b'.';
            dp = Some(count);
        } else if c == b'_' {
            if prev != b'0' {
                inval_sep = true;
            }
            prev = b'_';
        } else {
            let d = match c {
                b'0'..=b'9' => u32::from(c - b'0'),
                b'a'..=b'z' => u32::from(c - b'a') + 10,
                b'A'..=b'Z' => u32::from(c - b'A') + 10,
                _ => u32::MAX,
            };
            if d >= base {
                break;
            }
            prev = b'0';
            count += 1;
            mantissa.push(d as u8);
        }
        i += 1;
    }
    if inval_sep || prev == b'_' || count == 0 {
        return None;
    }
    Some(Mantissa {
        base,
        digits: mantissa,
        fcount: dp.map_or(0, |dp| dp - count),
        end: i,
    })
}

/// The powers of two and of five that the digits after the point of a
/// mantissa in `base` scale it by.
fn fraction_exponents(base: u32, fcount: i64) -> (i64, i64) {
    if fcount >= 0 {
        return (0, 0);
    }
    match base {
        10 => (fcount, fcount),
        2 => (fcount, 0),
        8 => (fcount * 3, 0),
        _ => (fcount * 4, 0),
    }
}

impl Number {
    /// Reads a number: an optional sign, digits with an optional fraction,
    /// and an optional exponent (`1.5e3`). A `0x`, `0o` or `0b` prefix
    /// selects another base, `p` introduces a binary exponent, and single
    /// underscores may separate digits. Words, `Inf`, `NaN` and fractions
    /// such as `1/2` are not numbers.
    pub fn parse(s: &str) -> Option<Number> {
        let b = s.as_bytes();
        if b.is_empty() || b.contains(&b'/') {
            return None;
        }
        let mut i = 0;
        let mut neg = false;
        if b[0] == b'+' || b[0] == b'-' {
            neg = b[0] == b'-';
            i = 1;
        }
        let Mantissa {
            base,
            digits: mantissa,
            fcount,
            end: i,
        } = scan_mantissa(b, i)?;
        // The exponent.
        let (exp, ebase) = match b.get(i) {
            None => (0, 10),
            Some(b'e' | b'E') => (scan_exponent(&b[i + 1..])?, 10),
            Some(b'p' | b'P') => (scan_exponent(&b[i + 1..])?, 2),
            Some(_) => return None,
        };
        if mantissa.iter().all(|&d| d == 0) {
            return Some(Number {
                neg: false,
                digits: Vec::new(),
                exp: 0,
            });
        }
        let (mut exp2, mut exp5) = fraction_exponents(base, fcount);
        if ebase == 10 {
            exp5 = exp5.checked_add(exp)?;
        }
        exp2 = exp2.checked_add(exp)?;
        if exp5.checked_abs()? > MAX_EXP5 || !(-MAX_EXP2..=MAX_EXP2).contains(&exp2) {
            return None;
        }
        // The value is mantissa × 10^exp5 × 2^residual.
        let residual = exp2 - exp5;
        let mut exp10 = exp5;
        let mut digits: Vec<u8> = if base == 10 && residual == 0 {
            mantissa.iter().map(|d| d + b'0').collect()
        } else {
            if residual.abs() > MAX_RESIDUAL {
                return None;
            }
            let mut n = Natural(Vec::new());
            for &d in &mantissa {
                n.mul_add(base, u32::from(d));
            }
            if residual >= 0 {
                n.mul_pow(2, residual);
            } else {
                // 2^-n is 5^n × 10^-n.
                n.mul_pow(5, -residual);
                exp10 += residual;
            }
            n.decimal()
        };
        let lead = digits.iter().take_while(|&&d| d == b'0').count();
        digits.drain(..lead);
        while digits.last() == Some(&b'0') {
            digits.pop();
            exp10 += 1;
        }
        Some(Number {
            neg,
            digits,
            exp: exp10,
        })
    }

    /// Whether the number has no fractional part.
    pub fn is_int(&self) -> bool {
        self.digits.is_empty() || self.exp >= 0
    }

    fn sign(&self) -> i32 {
        if self.digits.is_empty() {
            0
        } else if self.neg {
            -1
        } else {
            1
        }
    }
}

impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Number {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b) = (self.sign(), other.sign());
        if a != b || a == 0 {
            return a.cmp(&b);
        }
        // Same sign: compare the magnitudes by where the leading digit
        // sits, then digit by digit.
        let magnitude = (self.digits.len() as i64 + self.exp)
            .cmp(&(other.digits.len() as i64 + other.exp))
            .then_with(|| self.digits.cmp(&other.digits));
        if a < 0 {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Number {
        Number::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    #[test]
    fn parses_what_is_a_number() {
        for (a, b) in [
            ("0x10", "16"),
            ("1_000", "1000"),
            ("1e5", "100000"),
            ("0b11", "3"),
            ("017", "17"),
            ("1.5e-3", "0.0015"),
            (".5", "0.5"),
            ("5.", "5"),
            ("+1", "1"),
            ("1p3", "8"),
            ("0x1p3", "8"),
            ("0x1.8p1", "3"),
            ("0o17", "15"),
            ("0x_1", "1"),
            ("1e1_0", "10000000000"),
            ("-.5e1", "-5"),
            ("1.e2", "100"),
            ("00", "0"),
            ("08", "8"),
            ("0_8", "8"),
            ("0x1.8", "1.5"),
            ("1E5", "100000"),
            ("0X10", "16"),
            ("1e+5", "100000"),
            ("0b1.1p1", "3"),
            ("0b1.1", "1.5"),
            ("0o1.4p0", "1.5"),
            ("1.0", "1"),
            ("-0", "0"),
            ("1p-2", "0.25"),
        ] {
            assert_eq!(n(a), n(b), "{a} and {b}");
        }
        for bad in [
            "Inf",
            "1e",
            "1__0",
            " 1",
            "1/2",
            "0x",
            "--1",
            "",
            ".",
            "+",
            "1e2000000",
            "NaN",
            "1 ",
            "1e5x",
            "0b12",
        ] {
            assert!(Number::parse(bad).is_none(), "{bad:?} is not a number");
        }
        assert!(Number::parse("1e400000").is_some());
    }

    #[test]
    fn orders_exactly() {
        assert!(n("9007199254740993") > n("9007199254740992"));
        assert!(n("1e1000") > n("1e999"));
        assert!(n("1e-1000") < n("1"));
        assert!(n("-0.5") < n("+.5"));
        assert!(n("-2") < n("-1"));
        assert!(n("-1") < n("0"));
        assert!(n("0.25") <= n("0.25"));
        assert!(n("99") < n("1e2"));
        assert!(n("1.5") > n("1.25"));
        assert!(n("7").is_int() && n("1e3").is_int() && n("0").is_int() && n("10.0").is_int());
        assert!(!n("0.5").is_int() && !n("1e-3").is_int());
    }
}
