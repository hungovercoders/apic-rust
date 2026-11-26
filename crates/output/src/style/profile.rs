//! The colour profile: how many colours the output can show. Every style
//! is rendered against it, so one switch turns all of apic's output plain.

use std::cell::Cell;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// What a terminal can show, richest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    /// 24-bit colour.
    TrueColor,
    /// The 256-colour palette.
    Ansi256,
    /// The 16 basic colours.
    Ansi,
    /// No colour and no attributes: text comes out as it went in.
    Ascii,
}

impl Profile {
    /// The profile's name: `TrueColor`, `ANSI256`, `ANSI` or `Ascii`.
    pub fn name(self) -> &'static str {
        match self {
            Profile::TrueColor => "TrueColor",
            Profile::Ansi256 => "ANSI256",
            Profile::Ansi => "ANSI",
            Profile::Ascii => "Ascii",
        }
    }

    fn from_u8(n: u8) -> Option<Profile> {
        match n {
            0 => Some(Profile::TrueColor),
            1 => Some(Profile::Ansi256),
            2 => Some(Profile::Ansi),
            3 => Some(Profile::Ascii),
            _ => None,
        }
    }
}

const UNSET: u8 = u8::MAX;

static GLOBAL: AtomicU8 = AtomicU8::new(UNSET);
static DARK_BACKGROUND: AtomicBool = AtomicBool::new(true);

thread_local! {
    static OVERRIDE: Cell<Option<Profile>> = const { Cell::new(None) };
}

/// The profile styles render with: the one [`with_color_profile`] put in
/// force on this thread, else the one [`set_color_profile`] chose, else
/// the one the environment calls for ([`env_color_profile`]), worked out
/// once, on first use.
pub fn color_profile() -> Profile {
    if let Some(p) = OVERRIDE.with(Cell::get) {
        return p;
    }
    if let Some(p) = Profile::from_u8(GLOBAL.load(Ordering::Relaxed)) {
        return p;
    }
    let p = env_color_profile();
    // Another thread may have chosen meanwhile; its choice stands.
    match GLOBAL.compare_exchange(UNSET, p as u8, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => p,
        Err(now) => Profile::from_u8(now).unwrap_or(p),
    }
}

/// Sets the profile for the whole process. `Profile::Ascii` is what
/// --no-color, NO_COLOR, --json and output that is not a terminal ask for.
pub fn set_color_profile(p: Profile) {
    GLOBAL.store(p as u8, Ordering::Relaxed);
}

/// Runs `f` with `p` as this thread's profile, whatever the process-wide
/// one is, and restores what was in force before. It is how a test asks
/// for coloured or plain output without disturbing the tests running
/// beside it.
pub fn with_color_profile<R>(p: Profile, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Profile>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|o| o.set(self.0));
        }
    }
    let _restore = Restore(OVERRIDE.with(|o| o.replace(Some(p))));
    f()
}

/// The profile the environment calls for, for output on stdout: none when
/// NO_COLOR is set, CLICOLOR is 0 or stdout is not a terminal (or CI is
/// set), otherwise what TERM and COLORTERM promise. CLICOLOR_FORCE turns
/// the basic colours on where there would be none.
pub fn env_color_profile() -> Profile {
    detect_color_profile(std::io::stdout().is_terminal(), &|name| {
        std::env::var(name).unwrap_or_default()
    })
}

/// [`env_color_profile`] with its inputs passed in: whether the output is
/// a terminal, and the environment.
pub fn detect_color_profile(is_terminal: bool, getenv: &dyn Fn(&str) -> String) -> Profile {
    let forced = {
        let v = getenv("CLICOLOR_FORCE");
        !v.is_empty() && v != "0"
    };
    if !getenv("NO_COLOR").is_empty() || (getenv("CLICOLOR") == "0" && !forced) {
        return Profile::Ascii;
    }
    let is_tty = is_terminal && getenv("CI").is_empty();
    let p = if is_tty {
        terminal_profile(getenv)
    } else {
        Profile::Ascii
    };
    if forced && p == Profile::Ascii {
        return Profile::Ansi;
    }
    p
}

#[cfg(not(windows))]
fn terminal_profile(getenv: &dyn Fn(&str) -> String) -> Profile {
    if getenv("GOOGLE_CLOUD_SHELL") == "true" {
        return Profile::TrueColor;
    }
    let term = getenv("TERM");
    match getenv("COLORTERM").to_lowercase().as_str() {
        "24bit" | "truecolor" => {
            if term.starts_with("screen") && getenv("TERM_PROGRAM") != "tmux" {
                // tmux supports true colour, screen only 256.
                return Profile::Ansi256;
            }
            return Profile::TrueColor;
        }
        "yes" | "true" => return Profile::Ansi256,
        _ => {}
    }
    match term.as_str() {
        "alacritty" | "contour" | "rio" | "wezterm" | "xterm-ghostty" | "xterm-kitty" => {
            return Profile::TrueColor
        }
        "linux" | "xterm" => return Profile::Ansi,
        _ => {}
    }
    if term.contains("256color") {
        return Profile::Ansi256;
    }
    if term.contains("color") || term.contains("ansi") {
        return Profile::Ansi;
    }
    Profile::Ascii
}

/// Every console of a Windows that is still supported shows 24-bit
/// colour.
#[cfg(windows)]
fn terminal_profile(_getenv: &dyn Fn(&str) -> String) -> Profile {
    Profile::TrueColor
}

/// Whether the terminal's background is taken to be dark, which decides
/// the side of an adaptive colour. It is dark until
/// [`set_has_dark_background`] says otherwise: the terminal is never
/// asked, because a terminal that does not answer would stall start-up.
pub fn has_dark_background() -> bool {
    DARK_BACKGROUND.load(Ordering::Relaxed)
}

/// Says whether the terminal's background is dark.
pub fn set_has_dark_background(dark: bool) {
    DARK_BACKGROUND.store(dark, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn detect(tty: bool, env: &[(&str, &str)]) -> Profile {
        let env: HashMap<&str, &str> = env.iter().copied().collect();
        detect_color_profile(tty, &|k| {
            env.get(k).copied().unwrap_or_default().to_string()
        })
    }

    #[cfg(not(windows))]
    #[test]
    fn detects_the_profile_from_the_environment() {
        use Profile::*;
        type Case<'a> = (bool, &'a [(&'a str, &'a str)], Profile);
        let cases: &[Case] = &[
            (false, &[("TERM", "xterm-256color")], Ascii),
            (true, &[("TERM", "xterm-256color")], Ansi256),
            (true, &[("TERM", "xterm-256color"), ("CI", "true")], Ascii),
            (
                true,
                &[("TERM", "xterm-256color"), ("NO_COLOR", "1")],
                Ascii,
            ),
            (
                true,
                &[("TERM", "xterm-256color"), ("CLICOLOR", "0")],
                Ascii,
            ),
            (
                true,
                &[
                    ("TERM", "xterm-256color"),
                    ("CLICOLOR", "0"),
                    ("CLICOLOR_FORCE", "1"),
                ],
                Ansi256,
            ),
            (false, &[("CLICOLOR_FORCE", "1")], Ansi),
            (false, &[("CLICOLOR_FORCE", "0")], Ascii),
            (false, &[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")], Ascii),
            (
                true,
                &[("TERM", "xterm"), ("COLORTERM", "truecolor")],
                TrueColor,
            ),
            (
                true,
                &[("TERM", "xterm"), ("COLORTERM", "24BIT")],
                TrueColor,
            ),
            (
                true,
                &[("TERM", "screen"), ("COLORTERM", "truecolor")],
                Ansi256,
            ),
            (
                true,
                &[
                    ("TERM", "screen"),
                    ("COLORTERM", "truecolor"),
                    ("TERM_PROGRAM", "tmux"),
                ],
                TrueColor,
            ),
            (true, &[("TERM", "xterm"), ("COLORTERM", "yes")], Ansi256),
            (true, &[("TERM", "xterm-kitty")], TrueColor),
            (true, &[("TERM", "xterm")], Ansi),
            (true, &[("TERM", "linux")], Ansi),
            (true, &[("TERM", "rxvt-color")], Ansi),
            (true, &[("TERM", "ansi.sys")], Ansi),
            (true, &[("TERM", "dumb")], Ascii),
            (true, &[], Ascii),
            (true, &[("GOOGLE_CLOUD_SHELL", "true")], TrueColor),
        ];
        for (tty, env, want) in cases {
            assert_eq!(detect(*tty, env), *want, "tty={tty} env={env:?}");
        }
    }

    #[test]
    fn a_thread_override_wins_and_is_restored() {
        let before = color_profile();
        let inner = with_color_profile(Profile::Ansi, || {
            assert_eq!(color_profile(), Profile::Ansi);
            with_color_profile(Profile::TrueColor, color_profile)
        });
        assert_eq!(inner, Profile::TrueColor);
        assert_eq!(color_profile(), before);
    }
}
