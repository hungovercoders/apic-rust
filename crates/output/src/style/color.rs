//! Colours, and how one is brought down to what a profile can show: a hex
//! colour to the nearest of the 256, one of the 256 to the nearest of the
//! basic 16, by distance in the HSLuv colour space.

// The constants of the colour spaces are quoted as their definitions give
// them, digits beyond what a double holds included.
#![allow(clippy::excessive_precision)]

use super::profile::{has_dark_background, Profile};

/// A colour for a style. It is resolved against the colour profile when
/// the style is rendered, not when it is built.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// No colour: the terminal's default.
    #[default]
    None,
    /// An ANSI colour by number (`"5"`, `"212"`) or a hex colour
    /// (`"#ff87d7"`, `"#f8d"`). Numbers below 16 are the basic colours,
    /// which every colour terminal has; anything else is brought down to
    /// the nearest colour the profile can show.
    Value(String),
    /// One colour for a light background and one for a dark one.
    Adaptive { light: String, dark: String },
    /// A colour chosen by hand for each profile, with no conversion.
    Complete {
        true_color: String,
        ansi256: String,
        ansi: String,
    },
    /// [`Color::Complete`], for a light background and for a dark one.
    CompleteAdaptive { light: Box<Color>, dark: Box<Color> },
}

impl Color {
    /// A colour by ANSI number or hex value.
    pub fn new(value: impl Into<String>) -> Color {
        Color::Value(value.into())
    }

    /// One of the 256 ANSI colours by number.
    pub fn ansi(n: u8) -> Color {
        Color::Value(n.to_string())
    }

    /// One colour for a light background and one for a dark one.
    pub fn adaptive(light: impl Into<String>, dark: impl Into<String>) -> Color {
        Color::Adaptive {
            light: light.into(),
            dark: dark.into(),
        }
    }

    /// A colour chosen by hand for each profile.
    pub fn complete(
        true_color: impl Into<String>,
        ansi256: impl Into<String>,
        ansi: impl Into<String>,
    ) -> Color {
        Color::Complete {
            true_color: true_color.into(),
            ansi256: ansi256.into(),
            ansi: ansi.into(),
        }
    }

    /// The colour as the profile can show it; `None` when there is nothing
    /// to emit.
    pub(crate) fn resolve(&self, p: Profile) -> Option<TermColor> {
        match self {
            Color::None => Some(TermColor::NoColor),
            Color::Value(v) => profile_color(p, v),
            Color::Adaptive { light, dark } => {
                profile_color(p, if has_dark_background() { dark } else { light })
            }
            Color::Complete {
                true_color,
                ansi256,
                ansi,
            } => match p {
                Profile::TrueColor => profile_color(p, true_color),
                Profile::Ansi256 => profile_color(p, ansi256),
                Profile::Ansi => profile_color(p, ansi),
                Profile::Ascii => Some(TermColor::NoColor),
            },
            Color::CompleteAdaptive { light, dark } => {
                if has_dark_background() {
                    dark.resolve(p)
                } else {
                    light.resolve(p)
                }
            }
        }
    }

    /// The SGR parameters that select this colour under `p`, as foreground
    /// or background: `38;5;212`, `91`, `48;2;255;135;215`. Empty when the
    /// profile shows no colour or the value is not a colour.
    pub fn sequence(&self, p: Profile, background: bool) -> String {
        self.resolve(p)
            .map(|c| c.sequence(background))
            .unwrap_or_default()
    }
}

impl From<&str> for Color {
    fn from(v: &str) -> Color {
        Color::new(v)
    }
}

impl From<String> for Color {
    fn from(v: String) -> Color {
        Color::Value(v)
    }
}

impl From<u8> for Color {
    fn from(n: u8) -> Color {
        Color::ansi(n)
    }
}

/// A colour in the terms of one profile.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TermColor {
    NoColor,
    /// One of the basic 16.
    Ansi(i64),
    /// One of the 256.
    Ansi256(i64),
    /// `#rrggbb` or `#rgb`.
    Rgb(String),
}

impl TermColor {
    pub(crate) fn sequence(&self, bg: bool) -> String {
        match self {
            TermColor::NoColor => String::new(),
            TermColor::Ansi(c) => {
                let shift = if bg { 10 } else { 0 };
                if *c < 8 {
                    (c + shift + 30).to_string()
                } else {
                    (c - 8 + shift + 90).to_string()
                }
            }
            TermColor::Ansi256(c) => format!("{};5;{c}", if bg { "48" } else { "38" }),
            TermColor::Rgb(hex) => match parse_hex(hex) {
                None => String::new(),
                Some(c) => format!(
                    "{};2;{};{};{}",
                    if bg { "48" } else { "38" },
                    (c.r * 255.0) as u8,
                    (c.g * 255.0) as u8,
                    (c.b * 255.0) as u8
                ),
            },
        }
    }
}

/// Reads a colour value and converts it for the profile.
fn profile_color(p: Profile, s: &str) -> Option<TermColor> {
    if s.is_empty() {
        return None;
    }
    let c = if s.starts_with('#') {
        TermColor::Rgb(s.to_string())
    } else {
        let i = parse_int(s)?;
        if i < 16 {
            TermColor::Ansi(i)
        } else {
            TermColor::Ansi256(i)
        }
    };
    convert(p, c)
}

/// A decimal integer with an optional sign and nothing else.
fn parse_int(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn convert(p: Profile, c: TermColor) -> Option<TermColor> {
    if p == Profile::Ascii {
        return Some(TermColor::NoColor);
    }
    match c {
        TermColor::Ansi256(n) => {
            if p == Profile::Ansi {
                // A number beyond the palette is not a colour.
                let n = u8::try_from(n).ok()?;
                return Some(TermColor::Ansi(i64::from(ansi256_to_ansi(n))));
            }
            Some(TermColor::Ansi256(n))
        }
        TermColor::Rgb(hex) => {
            let h = parse_hex(&hex)?;
            if p != Profile::TrueColor {
                let ac = hex_to_ansi256(h);
                if p == Profile::Ansi {
                    return Some(TermColor::Ansi(i64::from(ansi256_to_ansi(ac))));
                }
                return Some(TermColor::Ansi256(i64::from(ac)));
            }
            Some(TermColor::Rgb(hex))
        }
        other => Some(other),
    }
}

/// A colour in sRGB, each channel from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rgb {
    r: f64,
    g: f64,
    b: f64,
}

/// Reads `#rrggbb` or `#rgb`.
fn parse_hex(s: &str) -> Option<Rgb> {
    let digits = s.strip_prefix('#')?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (n, factor) = match digits.len() {
        3 => (1, 1.0 / 15.0),
        6 => (2, 1.0 / 255.0),
        _ => return None,
    };
    let channel = |i: usize| -> Option<f64> {
        let v = u32::from_str_radix(&digits[i * n..(i + 1) * n], 16).ok()?;
        Some(f64::from(v) * factor)
    };
    Some(Rgb {
        r: channel(0)?,
        g: channel(1)?,
        b: channel(2)?,
    })
}

/// The basic 16 colours as most terminals show them.
const BASIC: [u32; 16] = [
    0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0x808080,
    0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
];

/// The steps of the 6x6x6 colour cube.
const CUBE: [u32; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

/// The colour of one of the 256 palette entries.
fn palette(n: u8) -> Rgb {
    let rgb = match n {
        0..=15 => BASIC[usize::from(n)],
        16..=231 => {
            let i = u32::from(n) - 16;
            CUBE[(i / 36) as usize] << 16 | CUBE[(i / 6 % 6) as usize] << 8 | CUBE[(i % 6) as usize]
        }
        _ => {
            let v = 8 + 10 * (u32::from(n) - 232);
            v << 16 | v << 8 | v
        }
    };
    let channel = |v: u32| f64::from(v & 0xff) * (1.0 / 255.0);
    Rgb {
        r: channel(rgb >> 16),
        g: channel(rgb >> 8),
        b: channel(rgb),
    }
}

/// The basic colour nearest to one of the 256.
fn ansi256_to_ansi(n: u8) -> u8 {
    let h = palette(n);
    let mut nearest = 0;
    let mut md = f64::MAX;
    for i in 0..=15 {
        let d = distance_hsluv(h, palette(i));
        if d < md {
            md = d;
            nearest = i;
        }
    }
    nearest
}

/// The palette colour nearest to an RGB colour: a cube colour or a grey.
fn hex_to_ansi256(c: Rgb) -> u8 {
    let v2ci = |v: f64| -> i32 {
        if v < 48.0 {
            0
        } else if v < 115.0 {
            1
        } else {
            ((v - 35.0) / 40.0) as i32
        }
    };
    // The nearest step of the cube, 0..5 on each axis.
    let r = v2ci(c.r * 255.0);
    let g = v2ci(c.g * 255.0);
    let b = v2ci(c.b * 255.0);
    let ci = 36 * r + 6 * g + b; // 0..215
    let cube = |i: i32| f64::from(CUBE[i.clamp(0, 5) as usize]) / 255.0;

    // The nearest grey. The average is taken over the cube steps, not the
    // channel values, so the candidate is always the darkest grey: that is
    // what terminals' colours have been matched against all along, and
    // output must not shift.
    let average = (r + g + b) / 3;
    let gray_idx = if average > 238 {
        23
    } else {
        (average - 3) / 10
    };
    let gv = f64::from(8 + 10 * gray_idx) / 255.0;

    let c2 = Rgb {
        r: cube(r),
        g: cube(g),
        b: cube(b),
    };
    let g2 = Rgb {
        r: gv,
        g: gv,
        b: gv,
    };
    if distance_hsluv(c, c2) <= distance_hsluv(c, g2) {
        (16 + ci) as u8
    } else {
        (232 + gray_idx) as u8
    }
}

fn sq(v: f64) -> f64 {
    v * v
}

fn linearize(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The white point HSLuv is defined against.
const HSLUV_D65: [f64; 3] = [0.950_455_927_051_67, 1.0, 1.089_057_750_759_878];

fn xyz_to_uv(x: f64, y: f64, z: f64) -> (f64, f64) {
    let denom = x + 15.0 * y + 3.0 * z;
    if denom == 0.0 {
        (0.0, 0.0)
    } else {
        (4.0 * x / denom, 9.0 * y / denom)
    }
}

/// sRGB to CIE L*C*h(uv), L and C from 0 to 1, h in degrees.
fn luv_lch(c: Rgb) -> (f64, f64, f64) {
    let (r, g, b) = (linearize(c.r), linearize(c.g), linearize(c.b));
    let x =
        0.412_390_799_265_959_48 * r + 0.357_584_339_383_877_96 * g + 0.180_480_788_401_834_29 * b;
    let y =
        0.212_639_005_871_510_36 * r + 0.715_168_678_767_755_93 * g + 0.072_192_315_360_733_715 * b;
    let z =
        0.019_330_818_715_591_851 * r + 0.119_194_779_794_625_99 * g + 0.950_532_152_249_660_58 * b;
    let wref = HSLUV_D65;
    let l = if y / wref[1] <= 6.0 / 29.0 * 6.0 / 29.0 * 6.0 / 29.0 {
        y / wref[1] * (29.0 / 3.0 * 29.0 / 3.0 * 29.0 / 3.0) / 100.0
    } else {
        1.16 * (y / wref[1]).cbrt() - 0.16
    };
    let (ubis, vbis) = xyz_to_uv(x, y, z);
    let (un, vn) = xyz_to_uv(wref[0], wref[1], wref[2]);
    let u = 13.0 * l * (ubis - un);
    let v = 13.0 * l * (vbis - vn);
    // u and v both next to nothing would make the angle noise.
    let h = if (v - u).abs() > 1e-4 && u.abs() > 1e-4 {
        (57.295_779_513_082_320_877_21 * v.atan2(u) + 360.0) % 360.0
    } else {
        0.0
    };
    (l, (sq(u) + sq(v)).sqrt(), h)
}

const M: [[f64; 3]; 3] = [
    [
        3.240_969_941_904_521_4,
        -1.537_383_177_570_093_5,
        -0.498_610_760_293_003_28,
    ],
    [
        -0.969_243_636_280_879_83,
        1.875_967_501_507_720_7,
        0.041_555_057_407_175_613,
    ],
    [
        0.055_630_079_696_993_609,
        -0.203_976_958_888_976_57,
        1.056_971_514_242_878_6,
    ],
];
const KAPPA: f64 = 903.296_296_296_296_3;
const EPSILON: f64 = 0.008_856_451_679_035_630_8;

fn max_chroma_for_lh(l: f64, h: f64) -> f64 {
    let h_rad = h / 360.0 * std::f64::consts::PI * 2.0;
    let sub1 = (l + 16.0).powf(3.0) / 1_560_896.0;
    let sub2 = if sub1 > EPSILON { sub1 } else { l / KAPPA };
    let mut min_length = f64::MAX;
    for m in &M {
        for k in 0..2 {
            let k = f64::from(k);
            let top1 = (284_517.0 * m[0] - 94_839.0 * m[2]) * sub2;
            let top2 = (838_422.0 * m[2] + 769_860.0 * m[1] + 731_718.0 * m[0]) * l * sub2
                - 769_860.0 * k * l;
            let bottom = (632_260.0 * m[2] - 126_452.0 * m[1]) * sub2 + 126_452.0 * k;
            let (x, y) = (top1 / bottom, top2 / bottom);
            let length = y / (h_rad.sin() - x * h_rad.cos());
            if length > 0.0 && length < min_length {
                min_length = length;
            }
        }
    }
    min_length
}

/// sRGB to HSLuv: hue in degrees, saturation and lightness from 0 to 1.
fn hsluv(c: Rgb) -> (f64, f64, f64) {
    let (l, c, h) = luv_lch(c);
    let c = c * 100.0;
    let l = l * 100.0;
    let s = if !(0.000_000_01..=99.999_999_9).contains(&l) {
        0.0
    } else {
        c / max_chroma_for_lh(l, h) * 100.0
    };
    let clamp01 = |v: f64| v.clamp(0.0, 1.0);
    (h, clamp01(s / 100.0), clamp01(l / 100.0))
}

fn distance_hsluv(c1: Rgb, c2: Rgb) -> f64 {
    let (h1, s1, l1) = hsluv(c1);
    let (h2, s2, l2) = hsluv(c2);
    (sq((h1 - h2) / 100.0) + sq(s1 - s2) + sq(l1 - l2)).sqrt()
}
