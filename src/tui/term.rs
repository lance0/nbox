//! Terminal color-capability detection.
//!
//! Splits the decision into a PURE resolver ([`color_mode`]) that takes the
//! relevant inputs as plain arguments — so it's exhaustively unit-testable
//! without touching the process environment — and thin impure wrappers
//! ([`no_color`], [`truecolor`], [`stdout_is_tty`], [`detect`]) that read the
//! environment / TTY state and call the resolver.
//!
//! Rules (in order):
//! 1. `NO_COLOR` set (to anything non-empty) ⇒ [`ColorMode::None`] (per
//!    <https://no-color.org>).
//! 2. stdout is not a TTY ⇒ [`ColorMode::None`] (don't emit color into a pipe).
//! 3. `COLORTERM`/`TERM` advertises truecolor (`truecolor` / `24bit`) ⇒
//!    [`ColorMode::TrueColor`].
//! 4. otherwise ⇒ [`ColorMode::Ansi`].

/// The color capability resolved for an output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// No color at all: `NO_COLOR` is set, or the stream isn't a terminal.
    None,
    /// Basic ANSI / 256-color: a terminal that didn't advertise 24-bit color.
    Ansi,
    /// 24-bit truecolor: `COLORTERM=truecolor`/`24bit` (or the same in `TERM`).
    TrueColor,
}

/// True when `COLORTERM`/`TERM` advertises 24-bit color. A value of `truecolor`
/// or `24bit` (case-insensitive, substring-matched on `TERM` for entries like
/// `xterm-truecolor`) signals truecolor support.
fn advertises_truecolor(colorterm: Option<&str>, term: Option<&str>) -> bool {
    let has = |v: &str| {
        let v = v.to_ascii_lowercase();
        v.contains("truecolor") || v.contains("24bit")
    };
    colorterm.is_some_and(has) || term.is_some_and(has)
}

/// The PURE color-mode resolver. Given the inputs that drive the decision — all
/// passed explicitly so this is testable without the real environment — return
/// the [`ColorMode`] per the rules documented on the module.
#[must_use]
pub fn color_mode(
    no_color: bool,
    stdout_is_tty: bool,
    colorterm: Option<&str>,
    term: Option<&str>,
) -> ColorMode {
    if no_color {
        return ColorMode::None;
    }
    if !stdout_is_tty {
        return ColorMode::None;
    }
    if advertises_truecolor(colorterm, term) {
        ColorMode::TrueColor
    } else {
        ColorMode::Ansi
    }
}

/// Whether `NO_COLOR` requests color be suppressed: set to any non-empty value
/// (per <https://no-color.org>, the variable's mere presence disables color, but
/// we treat an empty value as "unset" to avoid a stray `NO_COLOR=` killing color).
#[must_use]
pub fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// The `COLORTERM` environment value, if set (e.g. `truecolor`, `24bit`).
#[must_use]
pub fn colorterm() -> Option<String> {
    std::env::var("COLORTERM").ok()
}

/// The `TERM` environment value, if set (e.g. `xterm-256color`).
#[must_use]
pub fn term() -> Option<String> {
    std::env::var("TERM").ok()
}

/// Whether stdout is a terminal (reuses `is-terminal`, mirroring `update.rs`).
#[must_use]
pub fn stdout_is_tty() -> bool {
    use is_terminal::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Detect the effective [`ColorMode`] for stdout from the real environment: the
/// impure entry point that reads `NO_COLOR`/`COLORTERM`/`TERM` and the TTY state,
/// then defers to the pure [`color_mode`] resolver.
#[must_use]
pub fn detect() -> ColorMode {
    color_mode(
        no_color(),
        stdout_is_tty(),
        colorterm().as_deref(),
        term().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_forces_none_even_on_a_truecolor_tty() {
        // NO_COLOR wins over everything: a truecolor-advertising TTY still gets None.
        assert_eq!(
            color_mode(true, true, Some("truecolor"), Some("xterm-256color")),
            ColorMode::None
        );
    }

    #[test]
    fn not_a_tty_is_none() {
        // A pipe (not a TTY) gets no color, regardless of COLORTERM.
        assert_eq!(
            color_mode(false, false, Some("truecolor"), Some("xterm")),
            ColorMode::None
        );
    }

    #[test]
    fn colorterm_truecolor_is_truecolor() {
        assert_eq!(
            color_mode(false, true, Some("truecolor"), None),
            ColorMode::TrueColor
        );
    }

    #[test]
    fn colorterm_24bit_is_truecolor() {
        assert_eq!(
            color_mode(false, true, Some("24bit"), None),
            ColorMode::TrueColor
        );
    }

    #[test]
    fn colorterm_is_case_insensitive() {
        assert_eq!(
            color_mode(false, true, Some("TrueColor"), None),
            ColorMode::TrueColor
        );
    }

    #[test]
    fn term_can_advertise_truecolor() {
        // Some terminals only signal via TERM (e.g. xterm-truecolor).
        assert_eq!(
            color_mode(false, true, None, Some("xterm-truecolor")),
            ColorMode::TrueColor
        );
    }

    #[test]
    fn plain_tty_is_ansi() {
        // A TTY with an ordinary TERM and no truecolor hint is basic ANSI.
        assert_eq!(
            color_mode(false, true, None, Some("xterm-256color")),
            ColorMode::Ansi
        );
    }

    #[test]
    fn tty_with_no_hints_is_ansi() {
        // No COLORTERM and no TERM at all: still a TTY, so basic ANSI (not None).
        assert_eq!(color_mode(false, true, None, None), ColorMode::Ansi);
    }

    #[test]
    fn empty_colorterm_is_not_truecolor() {
        // A present-but-empty COLORTERM mustn't be read as truecolor.
        assert_eq!(color_mode(false, true, Some(""), None), ColorMode::Ansi);
    }
}
