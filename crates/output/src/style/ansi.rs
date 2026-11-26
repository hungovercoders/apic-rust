//! Measuring, cutting and wrapping text that carries escape sequences. A
//! sequence takes no room on screen, so it is never counted, and never
//! split: a cut line keeps the sequences after the cut, so a colour that
//! was switched on is still switched off.
//!
//! Widths are in terminal cells and counted per grapheme cluster: a letter
//! with its combining marks is one cell, a CJK character or an emoji two.

use unicode_width::UnicodeWidthChar;

use super::parser::{transition, COLLECT, EXECUTE, GROUND, PRINT, UTF8};

const NBSP: char = '\u{a0}';

fn is_control(c: char) -> bool {
    let n = c as u32;
    n < 0x20 || (0x7f..0xa0).contains(&n)
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

fn is_emoji_modifier(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

/// Whether a character is a pictograph a zero-width joiner can bind to
/// its neighbour, as in a family or a profession emoji.
fn is_pictographic(c: char) -> bool {
    matches!(c as u32,
        0xA9 | 0xAE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x2190..=0x21FF | 0x2300..=0x23FF | 0x2460..=0x24FF | 0x25A0..=0x27BF
        | 0x2900..=0x297F | 0x2B00..=0x2BFF | 0x1F000..=0x1FAFF)
        && !is_emoji_modifier(c)
        && !is_regional_indicator(c)
}

/// Whether a character is a vowel sign or the like that takes room of its
/// own yet belongs to the letter before it, in the Indic scripts, Thai,
/// Lao and halfwidth Katakana.
fn is_spacing_mark(c: char) -> bool {
    matches!(c as u32,
        0x0903 | 0x093B | 0x093E..=0x0940 | 0x0949..=0x094C | 0x094E..=0x094F
        | 0x0982..=0x0983 | 0x09BE..=0x09C0 | 0x09C7..=0x09C8 | 0x09CB..=0x09CC | 0x09D7
        | 0x0A03 | 0x0A3E..=0x0A40
        | 0x0A83 | 0x0ABE..=0x0AC0 | 0x0AC9 | 0x0ACB..=0x0ACC
        | 0x0B02..=0x0B03 | 0x0B3E | 0x0B40 | 0x0B47..=0x0B48 | 0x0B4B..=0x0B4C | 0x0B57
        | 0x0BBE..=0x0BBF | 0x0BC1..=0x0BC2 | 0x0BC6..=0x0BC8 | 0x0BCA..=0x0BCC | 0x0BD7
        | 0x0C01..=0x0C03 | 0x0C41..=0x0C44
        | 0x0C82..=0x0C83 | 0x0CBE | 0x0CC0..=0x0CC4 | 0x0CC7..=0x0CC8 | 0x0CCA..=0x0CCB
        | 0x0CD5..=0x0CD6
        | 0x0D02..=0x0D03 | 0x0D3E..=0x0D40 | 0x0D46..=0x0D48 | 0x0D4A..=0x0D4C | 0x0D57
        | 0x0D82..=0x0D83 | 0x0DCF..=0x0DD1 | 0x0DD8..=0x0DDF | 0x0DF2..=0x0DF3
        | 0x0E33 | 0x0EB3
        // The halfwidth voicing marks.
        | 0xFF9E..=0xFF9F)
}

/// Whether a character is the sign that binds two consonants of an Indic
/// script into a conjunct.
fn is_conjunct_linker(c: char) -> bool {
    matches!(
        c as u32,
        0x094D | 0x09CD | 0x0ACD | 0x0B4D | 0x0C4D | 0x0D4D
    )
}

/// Whether a character is a consonant of a script that forms conjuncts.
fn is_conjunct_consonant(c: char) -> bool {
    matches!(c as u32,
        0x0915..=0x0939 | 0x0958..=0x095F | 0x0978..=0x097F
        | 0x0995..=0x09B9 | 0x09DC..=0x09DF | 0x09F0..=0x09F1
        | 0x0A95..=0x0AB9 | 0x0AF9
        | 0x0B15..=0x0B39 | 0x0B5C..=0x0B5F | 0x0B71
        | 0x0C15..=0x0C39 | 0x0C58..=0x0C5A
        | 0x0D15..=0x0D3A)
}

/// Whether a character continues the cluster of the one before it: a
/// combining mark, a variation selector, a joiner, a trailing Hangul jamo,
/// a skin tone or a spacing vowel sign.
fn extends(c: char) -> bool {
    !is_control(c)
        && !is_trailing_jamo(c)
        && (c.width() == Some(0) || is_emoji_modifier(c) || is_spacing_mark(c))
}

/// Whether a character is a Hangul vowel or final consonant, which joins
/// the syllable before it and stands as a letter of its own otherwise.
fn is_trailing_jamo(c: char) -> bool {
    matches!(c as u32, 0x1160..=0x11FF | 0xD7B0..=0xD7C6 | 0xD7CB..=0xD7FB)
}

/// Whether a character is a Hangul leading consonant.
fn is_leading_jamo(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x115F | 0xA960..=0xA97C)
}

/// Whether a character is Hangul that a vowel or a final consonant can
/// follow: a leading consonant, a syllable, or a vowel or final itself.
fn is_hangul(c: char) -> bool {
    is_leading_jamo(c) || ('\u{AC00}'..='\u{D7A3}').contains(&c) || is_trailing_jamo(c)
}

/// The first grapheme cluster of `s` and its width in cells. The rules are
/// the common ones of the Unicode text segmentation annex: CR LF stay
/// together, a control character stands alone, combining marks and
/// joiners stay with their base, two regional indicators make a flag,
/// pictographs joined by a zero-width joiner make one emoji, and the
/// consonants of an Indic conjunct stay together.
pub fn first_grapheme_cluster(s: &str) -> (&str, usize) {
    let mut chars = s.char_indices();
    let Some((_, first)) = chars.next() else {
        return ("", 0);
    };
    let mut end = first.len_utf8();
    if first == '\r' {
        if s[end..].starts_with('\n') {
            end += 1;
        }
        return (&s[..end], 0);
    }
    if is_control(first) {
        return (&s[..end], 0);
    }
    let mut rest = s[end..].chars().peekable();
    if is_regional_indicator(first) {
        if let Some(&c) = rest.peek() {
            if is_regional_indicator(c) {
                end += c.len_utf8();
                rest.next();
            }
        }
    }
    let pictograph = is_pictographic(first);
    let consonant = is_conjunct_consonant(first);
    let hangul = is_hangul(first);
    let mut leading = is_leading_jamo(first);
    let (mut joiner, mut linker) = (false, false);
    while let Some(&c) = rest.peek() {
        if extends(c) {
            joiner = c == '\u{200D}';
            linker |= is_conjunct_linker(c);
        } else if (hangul && is_trailing_jamo(c)) || (leading && is_hangul(c)) {
            // The syllable goes on: leading consonants take more of them,
            // a vowel or a whole syllable; after those come only vowels
            // and finals.
            leading = is_leading_jamo(c);
        } else if joiner && pictograph && is_pictographic(c) {
            joiner = false;
        } else if linker && consonant && is_conjunct_consonant(c) {
            linker = false;
        } else {
            break;
        }
        end += c.len_utf8();
        rest.next();
    }
    let cluster = &s[..end];
    (cluster, cluster_width(cluster))
}

/// The width of one cluster: that of its first character, two when a
/// variation selector asks for the emoji form.
fn cluster_width(cluster: &str) -> usize {
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else {
        return 0;
    };
    if is_control(first) {
        return 0;
    }
    let w = if is_regional_indicator(first) {
        2
    } else if is_trailing_jamo(first) {
        1
    } else if matches!(first, '\u{2028}' | '\u{2029}') {
        0
    } else {
        first.width().unwrap_or(0)
    };
    if w < 2 && chars.next() == Some('\u{FE0F}') {
        return 2;
    }
    w
}

fn utf8_len(lead: u8) -> usize {
    match lead {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn text(buf: Vec<u8>) -> String {
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// The cluster at byte `i` of `s`; a position that is not a character
/// boundary (which the parser does not produce) yields one byte's worth.
fn cluster_at(s: &str, i: usize) -> (&str, usize) {
    match s.get(i..) {
        Some(rest) => first_grapheme_cluster(rest),
        None => ("", 0),
    }
}

/// Removes every escape sequence from `s`.
pub fn strip(s: &str) -> String {
    let b = s.as_bytes();
    let mut buf = Vec::with_capacity(b.len());
    let (mut ri, mut rw) = (0usize, 0usize);
    let mut pstate = GROUND;
    for &c in b {
        if pstate == UTF8 {
            // A character in progress: collect its remaining bytes.
            buf.push(c);
            ri += 1;
            if ri < rw {
                continue;
            }
            pstate = GROUND;
            ri = 0;
            rw = 0;
            continue;
        }
        let (state, action) = transition(pstate, c);
        match action {
            COLLECT => {
                if state == UTF8 {
                    rw = utf8_len(c);
                    buf.push(c);
                    ri += 1;
                }
            }
            PRINT | EXECUTE => buf.push(c),
            _ => {}
        }
        // Stay in the character until all of its bytes are in.
        if pstate != UTF8 {
            pstate = state;
        }
    }
    text(buf)
}

/// The width of `s` in terminal cells, escape sequences not counted. A
/// newline counts for nothing: measure a block line by line (see
/// [`super::width`]).
pub fn string_width(s: &str) -> usize {
    let b = s.as_bytes();
    let mut pstate = GROUND;
    let mut width = 0;
    let mut i = 0;
    while i < b.len() {
        let (state, action) = transition(pstate, b[i]);
        if action == PRINT || state == UTF8 {
            let (cluster, w) = cluster_at(s, i);
            width += w;
            i += cluster.len().max(1);
            pstate = GROUND;
            continue;
        }
        pstate = state;
        i += 1;
    }
    width
}

/// Cuts `s` to `length` cells, `tail` (an ellipsis, say) included, and
/// appends `tail` when anything was cut. Escape sequences are kept, those
/// after the cut too.
pub fn truncate(s: &str, length: i32, tail: &str) -> String {
    let length = i64::from(length);
    if string_width(s) as i64 <= length {
        return s.to_string();
    }
    let length = length - string_width(tail) as i64;
    if length < 0 {
        return String::new();
    }
    let length = length as usize;

    let b = s.as_bytes();
    let mut buf = Vec::with_capacity(b.len());
    let mut cur_width = 0;
    let mut ignoring = false;
    let mut pstate = GROUND;
    let mut i = 0;
    while i < b.len() {
        let (state, action) = transition(pstate, b[i]);
        if state == UTF8 {
            let (cluster, width) = cluster_at(s, i);
            i += cluster.len().max(1);
            cur_width += width;
            if ignoring {
                continue;
            }
            if cur_width > length {
                ignoring = true;
                buf.extend_from_slice(tail.as_bytes());
                continue;
            }
            buf.extend_from_slice(cluster.as_bytes());
            pstate = GROUND;
            continue;
        }
        match action {
            PRINT => {
                if cur_width >= length && !ignoring {
                    ignoring = true;
                    buf.extend_from_slice(tail.as_bytes());
                }
                if ignoring {
                    i += 1;
                    continue;
                }
                cur_width += 1;
                buf.push(b[i]);
                i += 1;
            }
            EXECUTE => {
                if ignoring {
                    i += 1;
                    continue;
                }
                buf.push(b[i]);
                i += 1;
            }
            _ => {
                buf.push(b[i]);
                i += 1;
            }
        }
        // Collect the bytes of the sequence in progress.
        pstate = state;
        if cur_width > length && !ignoring {
            ignoring = true;
            buf.extend_from_slice(tail.as_bytes());
        }
    }
    text(buf)
}

/// Cuts the first `n` cells off `s` and puts `prefix` in their place.
/// Escape sequences are kept.
pub fn truncate_left(s: &str, n: i32, prefix: &str) -> String {
    if n <= 0 {
        return s.to_string();
    }
    let n = n as usize;
    let b = s.as_bytes();
    let mut buf = Vec::with_capacity(b.len());
    let mut cur_width = 0;
    let mut ignoring = true;
    let mut pstate = GROUND;
    let mut i = 0;
    while i < b.len() {
        if !ignoring {
            buf.extend_from_slice(&b[i..]);
            break;
        }
        let (state, action) = transition(pstate, b[i]);
        if state == UTF8 {
            let (cluster, width) = cluster_at(s, i);
            i += cluster.len().max(1);
            cur_width += width;
            if cur_width > n && ignoring {
                ignoring = false;
                buf.extend_from_slice(prefix.as_bytes());
            }
            if cur_width > n {
                buf.extend_from_slice(cluster.as_bytes());
            }
            if ignoring {
                continue;
            }
            pstate = GROUND;
            continue;
        }
        match action {
            PRINT => {
                cur_width += 1;
                if cur_width > n && ignoring {
                    ignoring = false;
                    buf.extend_from_slice(prefix.as_bytes());
                }
                if ignoring {
                    i += 1;
                    continue;
                }
                buf.push(b[i]);
                i += 1;
            }
            EXECUTE => {
                if ignoring {
                    i += 1;
                    continue;
                }
                buf.push(b[i]);
                i += 1;
            }
            _ => {
                buf.push(b[i]);
                i += 1;
            }
        }
        pstate = state;
        if cur_width > n && ignoring {
            ignoring = false;
            buf.extend_from_slice(prefix.as_bytes());
        }
    }
    text(buf)
}

/// The cells of `s` from `left` up to `right`, escape sequences kept.
pub fn cut(s: &str, left: i32, right: i32) -> String {
    if right <= left {
        return String::new();
    }
    if left <= 0 {
        return truncate(s, right, "");
    }
    truncate_left(&truncate(s, right, ""), left, "")
}

fn byte_is_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ')
}

/// Breaks `s` every `limit` cells, wherever that falls. Unless
/// `preserve_space` is set, the white space a forced break lands on is
/// dropped from the start of the new line. A `limit` below one leaves `s`
/// alone.
pub fn hardwrap(s: &str, limit: i32, preserve_space: bool) -> String {
    if limit < 1 {
        return s.to_string();
    }
    let limit = limit as usize;
    let b = s.as_bytes();
    let mut buf = Vec::with_capacity(b.len() + b.len() / limit);
    let mut cur_width = 0;
    let mut force_newline = false;
    let mut pstate = GROUND;
    let mut i = 0;
    while i < b.len() {
        let (state, action) = transition(pstate, b[i]);
        if state == UTF8 {
            let (cluster, width) = cluster_at(s, i);
            i += cluster.len().max(1);
            if cur_width + width > limit {
                buf.push(b'\n');
                cur_width = 0;
            }
            if !preserve_space && cur_width == 0 && cluster.len() <= 4 {
                // Skip white space at the start of a line.
                if cluster.chars().next().is_some_and(char::is_whitespace) {
                    pstate = GROUND;
                    continue;
                }
            }
            buf.extend_from_slice(cluster.as_bytes());
            cur_width += width;
            pstate = GROUND;
            continue;
        }
        match action {
            PRINT | EXECUTE => 'byte: {
                if b[i] == b'\n' {
                    buf.push(b'\n');
                    cur_width = 0;
                    force_newline = false;
                    break 'byte;
                }
                if cur_width + 1 > limit {
                    buf.push(b'\n');
                    cur_width = 0;
                    force_newline = true;
                }
                // Skip white space at the start of a line.
                if cur_width == 0 {
                    if !preserve_space && force_newline && byte_is_space(b[i]) {
                        break 'byte;
                    }
                    force_newline = false;
                }
                buf.push(b[i]);
                if action == PRINT {
                    cur_width += 1;
                }
            }
            _ => buf.push(b[i]),
        }
        // Collect the bytes of the sequence in progress.
        if pstate != UTF8 {
            pstate = state;
        }
        i += 1;
    }
    text(buf)
}

fn contains_any(cluster: &str, set: &str) -> bool {
    cluster.chars().any(|c| set.contains(c))
}

/// Wraps `s` at word boundaries so that no line is wider than `limit`
/// cells, where a word fits; a word longer than a line is left whole. A
/// hyphen is a place to break, and so is every character of
/// `breakpoints`. A `limit` below one leaves `s` alone.
pub fn wordwrap(s: &str, limit: i32, breakpoints: &str) -> String {
    if limit < 1 {
        return s.to_string();
    }
    let limit = limit as usize;
    let b = s.as_bytes();
    let mut w = Wrapper::default();
    let mut pstate = GROUND;
    let mut i = 0;
    while i < b.len() {
        let (state, action) = transition(pstate, b[i]);
        if state == UTF8 {
            let (cluster, width) = cluster_at(s, i);
            i += cluster.len().max(1);
            let r = cluster
                .chars()
                .next()
                .unwrap_or(char::REPLACEMENT_CHARACTER);
            if r.is_whitespace() && r != NBSP {
                w.add_word();
                w.space.extend_from_slice(r.to_string().as_bytes());
                w.space_width += r.len_utf8();
            } else if contains_any(cluster, breakpoints) {
                w.add_space();
                w.add_word();
                w.buf.extend_from_slice(cluster.as_bytes());
                w.cur_width += 1;
            } else {
                w.word.extend_from_slice(cluster.as_bytes());
                w.word_len += width;
                if w.cur_width + w.space_width + w.word_len > limit && w.word_len < limit {
                    w.add_newline();
                }
            }
            pstate = GROUND;
            continue;
        }
        match action {
            PRINT | EXECUTE => {
                let c = b[i];
                if c == b'\n' {
                    if w.word_len == 0 {
                        if w.cur_width + w.space_width > limit {
                            w.cur_width = 0;
                        } else {
                            let space = std::mem::take(&mut w.space);
                            w.buf.extend_from_slice(&space);
                        }
                        w.clear_space();
                    }
                    w.add_word();
                    w.add_newline();
                } else if byte_is_space(c) {
                    w.add_word();
                    w.space.push(c);
                    w.space_width += 1;
                } else if c == b'-' || breakpoints.as_bytes().contains(&c) {
                    w.add_space();
                    w.add_word();
                    w.buf.push(c);
                    w.cur_width += 1;
                } else {
                    w.word.push(c);
                    w.word_len += 1;
                    if w.cur_width + w.space_width + w.word_len > limit && w.word_len < limit {
                        w.add_newline();
                    }
                }
            }
            _ => w.word.push(b[i]),
        }
        // Collect the bytes of the sequence in progress.
        if pstate != UTF8 {
            pstate = state;
        }
        i += 1;
    }
    w.add_word();
    text(w.buf)
}

/// Wraps `s` at word boundaries like [`wordwrap`], and breaks a word that
/// is longer than a line, so no line is ever wider than `limit` cells.
pub fn wrap(s: &str, limit: i32, breakpoints: &str) -> String {
    if limit < 1 {
        return s.to_string();
    }
    let limit = limit as usize;
    let b = s.as_bytes();
    let mut w = Wrapper::default();
    let mut pstate = GROUND;
    let mut i = 0;
    while i < b.len() {
        let (state, action) = transition(pstate, b[i]);
        if state == UTF8 {
            let (cluster, width) = cluster_at(s, i);
            i += cluster.len().max(1);
            let r = cluster
                .chars()
                .next()
                .unwrap_or(char::REPLACEMENT_CHARACTER);
            if r.is_whitespace() && r != NBSP {
                w.add_word();
                w.space.extend_from_slice(r.to_string().as_bytes());
                w.space_width += width;
            } else if contains_any(cluster, breakpoints) {
                w.add_space();
                if w.cur_width + w.word_len + width > limit {
                    w.word.extend_from_slice(cluster.as_bytes());
                    w.word_len += width;
                } else {
                    w.add_word();
                    w.buf.extend_from_slice(cluster.as_bytes());
                    w.cur_width += width;
                }
            } else {
                if w.word_len + width > limit {
                    // The word alone is wider than a line: break it.
                    w.add_word();
                }
                w.word.extend_from_slice(cluster.as_bytes());
                w.word_len += width;
                if w.cur_width + w.word_len + w.space_width > limit {
                    w.add_newline();
                }
                if w.word_len == limit {
                    // The word fills the line exactly: write it out.
                    w.add_word();
                }
            }
            pstate = GROUND;
            continue;
        }
        match action {
            PRINT | EXECUTE => {
                let c = b[i];
                if c == b'\n' {
                    w.flush_space(limit);
                    w.add_word();
                    w.add_newline();
                } else if byte_is_space(c) {
                    w.add_word();
                    w.space.push(c);
                    w.space_width += 1;
                } else if c == b'-' || breakpoints.as_bytes().contains(&c) {
                    w.add_space();
                    if w.cur_width + w.word_len >= limit {
                        // No room for the hyphen: it goes with the word.
                        w.word.push(c);
                        w.word_len += 1;
                    } else {
                        w.add_word();
                        w.buf.push(c);
                        w.cur_width += 1;
                    }
                } else {
                    if w.cur_width == limit {
                        w.add_newline();
                    }
                    w.word.push(c);
                    w.word_len += 1;
                    if w.word_len == limit {
                        w.add_word();
                    }
                    if w.cur_width + w.word_len + w.space_width > limit {
                        w.add_newline();
                    }
                }
            }
            _ => w.word.push(b[i]),
        }
        // Collect the bytes of the sequence in progress.
        if pstate != UTF8 {
            pstate = state;
        }
        i += 1;
    }
    w.flush_space(limit);
    w.add_word();
    text(w.buf)
}

/// The state the wrapping functions share: the text written so far, the
/// word and the white space waiting to be placed, and their widths.
#[derive(Default)]
struct Wrapper {
    buf: Vec<u8>,
    word: Vec<u8>,
    space: Vec<u8>,
    space_width: usize,
    cur_width: usize,
    /// Width of `word`, escape sequences not counted.
    word_len: usize,
}

impl Wrapper {
    fn clear_space(&mut self) {
        self.space.clear();
        self.space_width = 0;
    }

    fn add_space(&mut self) {
        self.cur_width += self.space_width;
        self.buf.append(&mut self.space);
        self.space_width = 0;
    }

    fn add_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        self.add_space();
        self.cur_width += self.word_len;
        self.buf.append(&mut self.word);
        self.word_len = 0;
    }

    fn add_newline(&mut self) {
        self.buf.push(b'\n');
        self.cur_width = 0;
        self.clear_space();
    }

    /// Writes the pending white space when no word follows it, unless it
    /// would overflow the line.
    fn flush_space(&mut self, limit: usize) {
        if self.word_len == 0 {
            if self.cur_width + self.space_width > limit {
                self.cur_width = 0;
            } else {
                self.buf.append(&mut self.space);
            }
            self.clear_space();
        }
    }
}
