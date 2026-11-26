//! The state machine that tells printable text from escape sequences: the
//! DEC ANSI parser, as a table of (state, byte) -> (next state, action).
//! The functions in `ansi` walk it byte by byte, so text is measured and
//! cut without ever counting or splitting a control sequence.

use std::sync::LazyLock;

pub(crate) const GROUND: u8 = 0;
const CSI_ENTRY: u8 = 1;
const CSI_INTERMEDIATE: u8 = 2;
const CSI_PARAM: u8 = 3;
const DCS_ENTRY: u8 = 4;
const DCS_INTERMEDIATE: u8 = 5;
const DCS_PARAM: u8 = 6;
const DCS_STRING: u8 = 7;
const ESCAPE: u8 = 8;
const ESCAPE_INTERMEDIATE: u8 = 9;
const OSC_STRING: u8 = 10;
const SOS_STRING: u8 = 11;
const PM_STRING: u8 = 12;
const APC_STRING: u8 = 13;
pub(crate) const UTF8: u8 = 14;

const NONE: u8 = 0;
const CLEAR: u8 = 1;
pub(crate) const COLLECT: u8 = 2;
const PREFIX: u8 = 3;
const DISPATCH: u8 = 4;
pub(crate) const EXECUTE: u8 = 5;
const START: u8 = 6;
const PUT: u8 = 7;
const PARAM: u8 = 8;
pub(crate) const PRINT: u8 = 9;
const IGNORE: u8 = NONE;

const ACTION_SHIFT: u8 = 4;
const STATE_MASK: u8 = 15;
const TABLE_SIZE: usize = 4096;

struct Table([u8; TABLE_SIZE]);

impl Table {
    fn one(&mut self, code: u8, state: u8, action: u8, next: u8) {
        self.0[usize::from(state) << 8 | usize::from(code)] = action << ACTION_SHIFT | next;
    }

    fn many(&mut self, codes: &[u8], state: u8, action: u8, next: u8) {
        for &code in codes {
            self.one(code, state, action, next);
        }
    }

    fn range(&mut self, start: u8, end: u8, state: u8, action: u8, next: u8) {
        for code in start..=end {
            self.one(code, state, action, next);
        }
    }
}

static TABLE: LazyLock<Table> = LazyLock::new(generate);

/// The next state and the action for `code` seen in `state`.
pub(crate) fn transition(state: u8, code: u8) -> (u8, u8) {
    let value = TABLE.0[usize::from(state) << 8 | usize::from(code)];
    (value & STATE_MASK, value >> ACTION_SHIFT)
}

/// The whole table, for the test that pins it.
#[cfg(test)]
pub(crate) fn table() -> &'static [u8] {
    &TABLE.0
}

fn generate() -> Table {
    let mut t = Table([NONE << ACTION_SHIFT | GROUND; TABLE_SIZE]);

    // Anywhere
    for state in GROUND..=UTF8 {
        // Anywhere -> Ground
        t.many(&[0x18, 0x1a, 0x99, 0x9a], state, EXECUTE, GROUND);
        t.range(0x80, 0x8F, state, EXECUTE, GROUND);
        t.range(0x90, 0x97, state, EXECUTE, GROUND);
        t.one(0x9C, state, EXECUTE, GROUND);
        // Anywhere -> Escape
        t.one(0x1B, state, CLEAR, ESCAPE);
        // Anywhere -> SosStringState
        t.one(0x98, state, START, SOS_STRING);
        // Anywhere -> PmStringState
        t.one(0x9E, state, START, PM_STRING);
        // Anywhere -> ApcStringState
        t.one(0x9F, state, START, APC_STRING);
        // Anywhere -> CsiEntry
        t.one(0x9B, state, CLEAR, CSI_ENTRY);
        // Anywhere -> DcsEntry
        t.one(0x90, state, CLEAR, DCS_ENTRY);
        // Anywhere -> OscString
        t.one(0x9D, state, START, OSC_STRING);
        // Anywhere -> Utf8
        t.range(0xC2, 0xDF, state, COLLECT, UTF8); // two bytes
        t.range(0xE0, 0xEF, state, COLLECT, UTF8); // three bytes
        t.range(0xF0, 0xF4, state, COLLECT, UTF8); // four bytes
    }

    // Ground
    t.range(0x00, 0x17, GROUND, EXECUTE, GROUND);
    t.one(0x19, GROUND, EXECUTE, GROUND);
    t.range(0x1C, 0x1F, GROUND, EXECUTE, GROUND);
    t.range(0x20, 0x7E, GROUND, PRINT, GROUND);
    t.one(0x7F, GROUND, EXECUTE, GROUND);

    // EscapeIntermediate
    t.range(
        0x00,
        0x17,
        ESCAPE_INTERMEDIATE,
        EXECUTE,
        ESCAPE_INTERMEDIATE,
    );
    t.one(0x19, ESCAPE_INTERMEDIATE, EXECUTE, ESCAPE_INTERMEDIATE);
    t.range(
        0x1C,
        0x1F,
        ESCAPE_INTERMEDIATE,
        EXECUTE,
        ESCAPE_INTERMEDIATE,
    );
    t.range(
        0x20,
        0x2F,
        ESCAPE_INTERMEDIATE,
        COLLECT,
        ESCAPE_INTERMEDIATE,
    );
    t.one(0x7F, ESCAPE_INTERMEDIATE, IGNORE, ESCAPE_INTERMEDIATE);
    // EscapeIntermediate -> Ground
    t.range(0x30, 0x7E, ESCAPE_INTERMEDIATE, DISPATCH, GROUND);

    // Escape
    t.range(0x00, 0x17, ESCAPE, EXECUTE, ESCAPE);
    t.one(0x19, ESCAPE, EXECUTE, ESCAPE);
    t.range(0x1C, 0x1F, ESCAPE, EXECUTE, ESCAPE);
    t.one(0x7F, ESCAPE, IGNORE, ESCAPE);
    // Escape -> Ground
    t.range(0x30, 0x4F, ESCAPE, DISPATCH, GROUND);
    t.range(0x51, 0x57, ESCAPE, DISPATCH, GROUND);
    t.one(0x59, ESCAPE, DISPATCH, GROUND);
    t.one(0x5A, ESCAPE, DISPATCH, GROUND);
    t.one(0x5C, ESCAPE, DISPATCH, GROUND);
    t.range(0x60, 0x7E, ESCAPE, DISPATCH, GROUND);
    // Escape -> EscapeIntermediate
    t.range(0x20, 0x2F, ESCAPE, COLLECT, ESCAPE_INTERMEDIATE);
    // Escape -> the strings
    t.one(b'X', ESCAPE, START, SOS_STRING);
    t.one(b'^', ESCAPE, START, PM_STRING);
    t.one(b'_', ESCAPE, START, APC_STRING);
    // Escape -> DcsEntry
    t.one(b'P', ESCAPE, CLEAR, DCS_ENTRY);
    // Escape -> CsiEntry
    t.one(b'[', ESCAPE, CLEAR, CSI_ENTRY);
    // Escape -> OscString
    t.one(b']', ESCAPE, START, OSC_STRING);

    // Sos, Pm and Apc strings
    for state in SOS_STRING..=APC_STRING {
        t.range(0x00, 0x17, state, PUT, state);
        t.one(0x19, state, PUT, state);
        t.range(0x1C, 0x1F, state, PUT, state);
        t.range(0x20, 0x7F, state, PUT, state);
        // ESC, ST, CAN and SUB end the string
        t.one(0x1B, state, DISPATCH, ESCAPE);
        t.one(0x9C, state, DISPATCH, GROUND);
        t.many(&[0x18, 0x1A], state, IGNORE, GROUND);
    }

    // DcsEntry
    t.range(0x00, 0x07, DCS_ENTRY, IGNORE, DCS_ENTRY);
    t.range(0x0E, 0x17, DCS_ENTRY, IGNORE, DCS_ENTRY);
    t.one(0x19, DCS_ENTRY, IGNORE, DCS_ENTRY);
    t.range(0x1C, 0x1F, DCS_ENTRY, IGNORE, DCS_ENTRY);
    t.one(0x7F, DCS_ENTRY, IGNORE, DCS_ENTRY);
    // DcsEntry -> DcsIntermediate
    t.range(0x20, 0x2F, DCS_ENTRY, COLLECT, DCS_INTERMEDIATE);
    // DcsEntry -> DcsParam
    t.range(0x30, 0x3B, DCS_ENTRY, PARAM, DCS_PARAM);
    t.range(0x3C, 0x3F, DCS_ENTRY, PREFIX, DCS_PARAM);
    // DcsEntry -> DcsPassThrough
    t.range(0x08, 0x0D, DCS_ENTRY, PUT, DCS_STRING); // ECMA-48 § 8.3.27
                                                     // ESC is let through, which the standard does not ask for, so a
                                                     // sequence wrapped in a DCS passes whole.
    t.one(0x1B, DCS_ENTRY, PUT, DCS_STRING);
    t.range(0x40, 0x7E, DCS_ENTRY, START, DCS_STRING);

    // DcsIntermediate
    t.range(0x00, 0x17, DCS_INTERMEDIATE, IGNORE, DCS_INTERMEDIATE);
    t.one(0x19, DCS_INTERMEDIATE, IGNORE, DCS_INTERMEDIATE);
    t.range(0x1C, 0x1F, DCS_INTERMEDIATE, IGNORE, DCS_INTERMEDIATE);
    t.range(0x20, 0x2F, DCS_INTERMEDIATE, COLLECT, DCS_INTERMEDIATE);
    t.one(0x7F, DCS_INTERMEDIATE, IGNORE, DCS_INTERMEDIATE);
    // DcsIntermediate -> DcsPassThrough
    t.range(0x30, 0x3F, DCS_INTERMEDIATE, START, DCS_STRING);
    t.range(0x40, 0x7E, DCS_INTERMEDIATE, START, DCS_STRING);

    // DcsParam
    t.range(0x00, 0x17, DCS_PARAM, IGNORE, DCS_PARAM);
    t.one(0x19, DCS_PARAM, IGNORE, DCS_PARAM);
    t.range(0x1C, 0x1F, DCS_PARAM, IGNORE, DCS_PARAM);
    t.range(0x30, 0x3B, DCS_PARAM, PARAM, DCS_PARAM);
    t.one(0x7F, DCS_PARAM, IGNORE, DCS_PARAM);
    t.range(0x3C, 0x3F, DCS_PARAM, IGNORE, DCS_PARAM);
    // DcsParam -> DcsIntermediate
    t.range(0x20, 0x2F, DCS_PARAM, COLLECT, DCS_INTERMEDIATE);
    // DcsParam -> DcsPassThrough
    t.range(0x40, 0x7E, DCS_PARAM, START, DCS_STRING);

    // DcsPassThrough
    t.range(0x00, 0x17, DCS_STRING, PUT, DCS_STRING);
    t.one(0x19, DCS_STRING, PUT, DCS_STRING);
    t.range(0x1C, 0x1F, DCS_STRING, PUT, DCS_STRING);
    t.range(0x20, 0x7E, DCS_STRING, PUT, DCS_STRING);
    t.one(0x7F, DCS_STRING, PUT, DCS_STRING);
    t.range(0x80, 0xFF, DCS_STRING, PUT, DCS_STRING); // UTF-8 text
                                                      // ST, CAN, SUB and ESC end the string
    t.one(0x1B, DCS_STRING, DISPATCH, ESCAPE);
    t.one(0x9C, DCS_STRING, DISPATCH, GROUND);
    t.many(&[0x18, 0x1A], DCS_STRING, IGNORE, GROUND);

    // CsiParam
    t.range(0x00, 0x17, CSI_PARAM, EXECUTE, CSI_PARAM);
    t.one(0x19, CSI_PARAM, EXECUTE, CSI_PARAM);
    t.range(0x1C, 0x1F, CSI_PARAM, EXECUTE, CSI_PARAM);
    t.range(0x30, 0x3B, CSI_PARAM, PARAM, CSI_PARAM);
    t.one(0x7F, CSI_PARAM, IGNORE, CSI_PARAM);
    t.range(0x3C, 0x3F, CSI_PARAM, IGNORE, CSI_PARAM);
    // CsiParam -> Ground
    t.range(0x40, 0x7E, CSI_PARAM, DISPATCH, GROUND);
    // CsiParam -> CsiIntermediate
    t.range(0x20, 0x2F, CSI_PARAM, COLLECT, CSI_INTERMEDIATE);

    // CsiIntermediate
    t.range(0x00, 0x17, CSI_INTERMEDIATE, EXECUTE, CSI_INTERMEDIATE);
    t.one(0x19, CSI_INTERMEDIATE, EXECUTE, CSI_INTERMEDIATE);
    t.range(0x1C, 0x1F, CSI_INTERMEDIATE, EXECUTE, CSI_INTERMEDIATE);
    t.range(0x20, 0x2F, CSI_INTERMEDIATE, COLLECT, CSI_INTERMEDIATE);
    t.one(0x7F, CSI_INTERMEDIATE, IGNORE, CSI_INTERMEDIATE);
    // CsiIntermediate -> Ground
    t.range(0x40, 0x7E, CSI_INTERMEDIATE, DISPATCH, GROUND);
    // CsiIntermediate -> CsiIgnore
    t.range(0x30, 0x3F, CSI_INTERMEDIATE, IGNORE, GROUND);

    // CsiEntry
    t.range(0x00, 0x17, CSI_ENTRY, EXECUTE, CSI_ENTRY);
    t.one(0x19, CSI_ENTRY, EXECUTE, CSI_ENTRY);
    t.range(0x1C, 0x1F, CSI_ENTRY, EXECUTE, CSI_ENTRY);
    t.one(0x7F, CSI_ENTRY, IGNORE, CSI_ENTRY);
    // CsiEntry -> Ground
    t.range(0x40, 0x7E, CSI_ENTRY, DISPATCH, GROUND);
    // CsiEntry -> CsiIntermediate
    t.range(0x20, 0x2F, CSI_ENTRY, COLLECT, CSI_INTERMEDIATE);
    // CsiEntry -> CsiParam
    t.range(0x30, 0x3B, CSI_ENTRY, PARAM, CSI_PARAM);
    t.range(0x3C, 0x3F, CSI_ENTRY, PREFIX, CSI_PARAM);

    // OscString
    t.range(0x00, 0x06, OSC_STRING, IGNORE, OSC_STRING);
    t.range(0x08, 0x17, OSC_STRING, IGNORE, OSC_STRING);
    t.one(0x19, OSC_STRING, IGNORE, OSC_STRING);
    t.range(0x1C, 0x1F, OSC_STRING, IGNORE, OSC_STRING);
    t.range(0x20, 0xFF, OSC_STRING, PUT, OSC_STRING); // UTF-8 text
                                                      // ST, CAN, SUB, ESC and BEL end the string
    t.one(0x1B, OSC_STRING, DISPATCH, ESCAPE);
    t.one(0x07, OSC_STRING, DISPATCH, GROUND);
    t.one(0x9C, OSC_STRING, DISPATCH, GROUND);
    t.many(&[0x18, 0x1A], OSC_STRING, IGNORE, GROUND);

    t
}
