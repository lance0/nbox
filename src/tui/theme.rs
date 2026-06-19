//! Color theme definitions for the TUI.
//!
//! Provides 12 built-in themes: default, kawaii, cyber, dracula, monochrome,
//! matrix, nord, gruvbox, catppuccin, tokyo_night, solarized, light. The active theme
//! comes from `[ui].theme` in the config and is cycled with `t` in the TUI
//! (Phase 3). Ported from the xfr/ttl theme system.

use ratatui::style::{Color, Style};
use std::borrow::Cow;

/// The severity of a transient message (the status/footer line). Maps to one of
/// the theme's success/warning/error colors, or neutral for ordinary chatter.
/// Kept separate from the message text so the text→color mapping lives in one
/// pure, testable place ([`Theme::message_style`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// Ordinary, non-alarming status (searching…, loading…, theme: nord).
    #[default]
    Info,
    /// A completed action worth confirming (copied, refreshed).
    Success,
    /// A degraded / partial result (some endpoints failed, partial search).
    Warning,
    /// A failure (request error, load failure).
    Error,
}

/// All themeable colors in the application.
#[derive(Clone, Debug)]
pub struct Theme {
    name: Cow<'static, str>,

    /// When set, the theme renders monochrome: every color field is
    /// [`Color::Reset`] and the style accessors return unstyled output. Driven by
    /// `NO_COLOR` at TUI startup (see [`Theme::no_color`]). Kept on the theme so
    /// `Theme` stays the single source of truth for color decisions.
    no_color: bool,

    // UI chrome
    pub border: Color,
    pub border_focused: Color,
    pub text: Color,
    pub text_dim: Color,
    pub highlight_bg: Color,
    /// A subtle background fill for the header/footer status bars, kept distinct
    /// from `highlight_bg` (row selection) so chrome and selection never blend.
    pub chrome_bg: Color,

    // Status indicators
    pub success: Color,
    pub warning: Color,
    pub error: Color,

    // Accents
    pub accent: Color,
    pub header: Color,

    // Graph / utilization colors
    pub graph_primary: Color,
    pub graph_secondary: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_theme()
    }
}

impl Theme {
    /// The default theme.
    pub fn default_theme() -> Self {
        Self {
            name: Cow::Borrowed("default"),
            no_color: false,

            border: Color::Cyan,
            border_focused: Color::Cyan,
            text: Color::White,
            text_dim: Color::Gray,
            highlight_bg: Color::DarkGray,
            chrome_bg: Color::Rgb(28, 28, 38),

            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,

            accent: Color::Yellow,
            header: Color::Cyan,

            graph_primary: Color::Green,
            graph_secondary: Color::Cyan,
        }
    }

    /// Kawaii theme - cute pastel colors.
    pub fn kawaii() -> Self {
        Self {
            name: Cow::Borrowed("kawaii"),
            no_color: false,

            border: Color::Rgb(255, 182, 214),
            border_focused: Color::Rgb(255, 182, 214),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(180, 180, 200),
            highlight_bg: Color::Rgb(60, 50, 70),
            chrome_bg: Color::Rgb(45, 38, 52),

            success: Color::Rgb(152, 255, 200),
            warning: Color::Rgb(255, 200, 152),
            error: Color::Rgb(255, 121, 162),

            accent: Color::Rgb(214, 182, 255),
            header: Color::Rgb(255, 182, 214),

            graph_primary: Color::Rgb(152, 255, 200),
            graph_secondary: Color::Rgb(255, 182, 214),
        }
    }

    /// Cyber/Futuristic theme - neon on dark.
    pub fn cyber() -> Self {
        Self {
            name: Cow::Borrowed("cyber"),
            no_color: false,

            border: Color::Rgb(0, 255, 255),
            border_focused: Color::Rgb(0, 255, 255),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(100, 100, 120),
            highlight_bg: Color::Rgb(20, 20, 35),
            chrome_bg: Color::Rgb(12, 12, 24),

            success: Color::Rgb(0, 255, 150),
            warning: Color::Rgb(255, 200, 0),
            error: Color::Rgb(255, 50, 100),

            accent: Color::Rgb(255, 0, 255),
            header: Color::Rgb(0, 255, 255),

            graph_primary: Color::Rgb(0, 255, 150),
            graph_secondary: Color::Rgb(0, 255, 255),
        }
    }

    /// Dracula theme.
    pub fn dracula() -> Self {
        Self {
            name: Cow::Borrowed("dracula"),
            no_color: false,

            border: Color::Rgb(189, 147, 249),
            border_focused: Color::Rgb(189, 147, 249),
            text: Color::Rgb(248, 248, 242),
            text_dim: Color::Rgb(98, 114, 164),
            highlight_bg: Color::Rgb(68, 71, 90),
            chrome_bg: Color::Rgb(40, 42, 54),

            success: Color::Rgb(80, 250, 123),
            warning: Color::Rgb(255, 184, 108),
            error: Color::Rgb(255, 85, 85),

            accent: Color::Rgb(241, 250, 140),
            header: Color::Rgb(255, 121, 198),

            graph_primary: Color::Rgb(80, 250, 123),
            graph_secondary: Color::Rgb(189, 147, 249),
        }
    }

    /// Monochrome theme - grayscale only.
    pub fn monochrome() -> Self {
        Self {
            name: Cow::Borrowed("monochrome"),
            no_color: false,

            border: Color::Rgb(200, 200, 200),
            border_focused: Color::Rgb(200, 200, 200),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(120, 120, 120),
            highlight_bg: Color::Rgb(50, 50, 50),
            chrome_bg: Color::Rgb(38, 38, 38),

            success: Color::Rgb(200, 200, 200),
            warning: Color::Rgb(170, 170, 170),
            error: Color::Rgb(255, 255, 255),

            accent: Color::Rgb(200, 200, 200),
            header: Color::Rgb(255, 255, 255),

            graph_primary: Color::Rgb(200, 200, 200),
            graph_secondary: Color::Rgb(150, 150, 150),
        }
    }

    /// Matrix theme - green on black hacker style.
    pub fn matrix() -> Self {
        Self {
            name: Cow::Borrowed("matrix"),
            no_color: false,

            border: Color::Rgb(0, 255, 0),
            border_focused: Color::Rgb(0, 255, 0),
            text: Color::Rgb(0, 255, 0),
            text_dim: Color::Rgb(0, 100, 0),
            highlight_bg: Color::Rgb(0, 20, 0),
            chrome_bg: Color::Rgb(0, 15, 0),

            success: Color::Rgb(0, 255, 0),
            warning: Color::Rgb(200, 255, 100),
            error: Color::Rgb(255, 100, 100),

            accent: Color::Rgb(100, 255, 100),
            header: Color::Rgb(0, 255, 0),

            graph_primary: Color::Rgb(0, 255, 0),
            graph_secondary: Color::Rgb(0, 200, 0),
        }
    }

    /// Nord theme - arctic, north-bluish colors.
    pub fn nord() -> Self {
        Self {
            name: Cow::Borrowed("nord"),
            no_color: false,

            border: Color::Rgb(136, 192, 208),
            border_focused: Color::Rgb(136, 192, 208),
            text: Color::Rgb(236, 239, 244),
            text_dim: Color::Rgb(76, 86, 106),
            highlight_bg: Color::Rgb(59, 66, 82),
            chrome_bg: Color::Rgb(46, 52, 64),

            success: Color::Rgb(163, 190, 140),
            warning: Color::Rgb(235, 203, 139),
            error: Color::Rgb(191, 97, 106),

            accent: Color::Rgb(235, 203, 139),
            header: Color::Rgb(136, 192, 208),

            graph_primary: Color::Rgb(163, 190, 140),
            graph_secondary: Color::Rgb(136, 192, 208),
        }
    }

    /// Gruvbox theme - retro groove colors.
    pub fn gruvbox() -> Self {
        Self {
            name: Cow::Borrowed("gruvbox"),
            no_color: false,

            border: Color::Rgb(254, 128, 25),
            border_focused: Color::Rgb(254, 128, 25),
            text: Color::Rgb(235, 219, 178),
            text_dim: Color::Rgb(146, 131, 116),
            highlight_bg: Color::Rgb(80, 73, 69),
            chrome_bg: Color::Rgb(40, 40, 40),

            success: Color::Rgb(184, 187, 38),
            warning: Color::Rgb(250, 189, 47),
            error: Color::Rgb(251, 73, 52),

            accent: Color::Rgb(250, 189, 47),
            header: Color::Rgb(254, 128, 25),

            graph_primary: Color::Rgb(184, 187, 38),
            graph_secondary: Color::Rgb(254, 128, 25),
        }
    }

    /// Catppuccin Mocha theme - soothing pastel colors.
    pub fn catppuccin() -> Self {
        Self {
            name: Cow::Borrowed("catppuccin"),
            no_color: false,

            border: Color::Rgb(203, 166, 247),
            border_focused: Color::Rgb(203, 166, 247),
            text: Color::Rgb(205, 214, 244),
            text_dim: Color::Rgb(108, 112, 134),
            highlight_bg: Color::Rgb(88, 91, 112),
            chrome_bg: Color::Rgb(30, 30, 46),

            success: Color::Rgb(166, 227, 161),
            warning: Color::Rgb(249, 226, 175),
            error: Color::Rgb(243, 139, 168),

            accent: Color::Rgb(249, 226, 175),
            header: Color::Rgb(245, 194, 231),

            graph_primary: Color::Rgb(166, 227, 161),
            graph_secondary: Color::Rgb(203, 166, 247),
        }
    }

    /// Tokyo Night theme.
    pub fn tokyo_night() -> Self {
        Self {
            name: Cow::Borrowed("tokyo_night"),
            no_color: false,

            border: Color::Rgb(187, 154, 247),
            border_focused: Color::Rgb(187, 154, 247),
            text: Color::Rgb(192, 202, 245),
            text_dim: Color::Rgb(86, 95, 137),
            highlight_bg: Color::Rgb(59, 66, 97),
            chrome_bg: Color::Rgb(26, 27, 38),

            success: Color::Rgb(158, 206, 106),
            warning: Color::Rgb(224, 175, 104),
            error: Color::Rgb(247, 118, 142),

            accent: Color::Rgb(224, 175, 104),
            header: Color::Rgb(187, 154, 247),

            graph_primary: Color::Rgb(158, 206, 106),
            graph_secondary: Color::Rgb(187, 154, 247),
        }
    }

    /// Solarized Dark theme.
    pub fn solarized() -> Self {
        Self {
            name: Cow::Borrowed("solarized"),
            no_color: false,

            border: Color::Rgb(42, 161, 152),
            border_focused: Color::Rgb(42, 161, 152),
            text: Color::Rgb(131, 148, 150),
            text_dim: Color::Rgb(88, 110, 117),
            highlight_bg: Color::Rgb(7, 54, 66),
            chrome_bg: Color::Rgb(0, 43, 54),

            success: Color::Rgb(133, 153, 0),
            warning: Color::Rgb(181, 137, 0),
            error: Color::Rgb(220, 50, 47),

            accent: Color::Rgb(181, 137, 0),
            header: Color::Rgb(203, 75, 22),

            graph_primary: Color::Rgb(133, 153, 0),
            graph_secondary: Color::Rgb(42, 161, 152),
        }
    }

    /// Solarized Light theme — the only light-background theme. Dark ink on a
    /// paper background: foreground/border colors are dark enough to read on a
    /// light terminal bg, the selection highlight is a light tan (`base2`) that
    /// keeps the dark text legible, and `border_focused` is a strong blue so the
    /// focused pane stands out. The success/warning/error and accent colors are
    /// Solarized's own green/amber/red, all of which read well on white — unlike
    /// the dark themes' bright accents, which wash out on a light terminal.
    pub fn light() -> Self {
        Self {
            name: Cow::Borrowed("light"),
            no_color: false,

            border: Color::Rgb(147, 161, 161),
            border_focused: Color::Rgb(38, 139, 210),
            text: Color::Rgb(101, 123, 131),
            text_dim: Color::Rgb(147, 161, 161),
            highlight_bg: Color::Rgb(238, 232, 213),
            chrome_bg: Color::Rgb(238, 232, 213),

            success: Color::Rgb(133, 153, 0),
            warning: Color::Rgb(181, 137, 0),
            error: Color::Rgb(220, 50, 47),

            accent: Color::Rgb(181, 137, 0),
            header: Color::Rgb(38, 139, 210),

            graph_primary: Color::Rgb(133, 153, 0),
            graph_secondary: Color::Rgb(42, 161, 152),
        }
    }

    /// A monochrome theme that honors `NO_COLOR`: every color field is
    /// [`Color::Reset`] (the terminal's own default fg/bg, no styling) and the
    /// style accessors ([`Theme::message_style`], [`Theme::status_style`]) return
    /// unstyled output. Selected at TUI startup when `NO_COLOR` is set, so the
    /// whole UI renders without color while remaining fully usable (selection is
    /// still marked by the `>` cursor, not a background highlight).
    ///
    /// This is distinct from the named [`Theme::monochrome`] theme, which is a
    /// grayscale *RGB* palette (still color); `no_color` emits no color at all.
    pub fn no_color() -> Self {
        Self {
            name: Cow::Borrowed("no_color"),
            no_color: true,

            border: Color::Reset,
            border_focused: Color::Reset,
            text: Color::Reset,
            text_dim: Color::Reset,
            highlight_bg: Color::Reset,
            chrome_bg: Color::Reset,

            success: Color::Reset,
            warning: Color::Reset,
            error: Color::Reset,

            accent: Color::Reset,
            header: Color::Reset,

            graph_primary: Color::Reset,
            graph_secondary: Color::Reset,
        }
    }

    /// Whether this theme is the monochrome `NO_COLOR` mode (see
    /// [`Theme::no_color`]). The render path can use this to skip even non-color
    /// styling (bold/reverse) if it wants a truly bare output; today the color
    /// fields being [`Color::Reset`] already suffice.
    pub fn is_no_color(&self) -> bool {
        self.no_color
    }

    /// Get a theme by name (case-insensitive, with aliases). Unknown → default.
    pub fn by_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "kawaii" => Self::kawaii(),
            "cyber" | "futuristic" => Self::cyber(),
            "monochrome" | "mono" => Self::monochrome(),
            "dracula" => Self::dracula(),
            "matrix" | "hacker" => Self::matrix(),
            "nord" => Self::nord(),
            "gruvbox" => Self::gruvbox(),
            "catppuccin" | "mocha" => Self::catppuccin(),
            "tokyo_night" | "tokyo" | "tokyonight" => Self::tokyo_night(),
            "solarized" => Self::solarized(),
            "light" | "day" => Self::light(),
            _ => Self::default_theme(),
        }
    }

    /// The theme's canonical name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// All available theme names, in cycle order.
    pub fn list() -> &'static [&'static str] {
        &[
            "default",
            "kawaii",
            "cyber",
            "dracula",
            "monochrome",
            "matrix",
            "nord",
            "gruvbox",
            "catppuccin",
            "tokyo_night",
            "solarized",
            "light",
        ]
    }

    /// The index of this theme's name within [`Theme::list`], or 0 (default).
    pub fn index_of(name: &str) -> usize {
        Self::list()
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .unwrap_or(0)
    }

    /// The style a transient message of a given [`Severity`] renders in: the
    /// theme's success/warning/error color, or neutral (dim) text for `Info`.
    /// The single place the severity→color mapping lives (pure, testable).
    pub fn message_style(&self, severity: Severity) -> Style {
        if self.no_color {
            return Style::default();
        }
        let color = match severity {
            Severity::Info => self.text_dim,
            Severity::Success => self.success,
            Severity::Warning => self.warning,
            Severity::Error => self.error,
        };
        Style::default().fg(color)
    }

    /// The style a NetBox object status (`active`, `offline`, `planned`, …)
    /// renders in, mapped to the theme's palette by severity. Unknown statuses
    /// stay neutral (the theme's normal text). Case-insensitive. The status
    /// TEXT is never changed — only its color. Pure and testable.
    pub fn status_style(&self, status: &str) -> Style {
        if self.no_color {
            return Style::default();
        }
        let color = match status.trim().to_ascii_lowercase().as_str() {
            // Healthy / in-service.
            "active" | "connected" | "reserved" => self.success,
            // In-progress / not-yet-live.
            "planned" | "staged" | "staging" | "provisioning" | "offered" => self.warning,
            // Out-of-service / failed / retiring.
            "offline" | "deprecated" | "failed" | "decommissioning" | "deprovisioning"
            | "retired" | "dhcp" => self.error,
            // Anything else: leave neutral.
            _ => self.text,
        };
        Style::default().fg(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_default() {
        assert_eq!(Theme::by_name("default").name(), "default");
    }

    #[test]
    fn by_name_unknown_returns_default() {
        assert_eq!(Theme::by_name("nope").name(), "default");
    }

    #[test]
    fn by_name_is_case_insensitive() {
        assert_eq!(
            Theme::by_name("KAWAII").name(),
            Theme::by_name("kawaii").name()
        );
    }

    #[test]
    fn all_listed_themes_load_and_match_their_name() {
        for name in Theme::list() {
            assert_eq!(Theme::by_name(name).name(), *name);
        }
    }

    #[test]
    fn index_of_round_trips_with_list() {
        for (i, name) in Theme::list().iter().enumerate() {
            assert_eq!(Theme::index_of(name), i);
        }
        assert_eq!(Theme::index_of("unknown"), 0);
    }

    #[test]
    fn message_style_maps_each_severity_to_its_theme_color() {
        let t = Theme::default_theme();
        assert_eq!(t.message_style(Severity::Success).fg, Some(t.success));
        assert_eq!(t.message_style(Severity::Warning).fg, Some(t.warning));
        assert_eq!(t.message_style(Severity::Error).fg, Some(t.error));
        // Neutral chatter is dim, not one of the alarming colors.
        assert_eq!(t.message_style(Severity::Info).fg, Some(t.text_dim));
    }

    #[test]
    fn severity_default_is_info() {
        assert_eq!(Severity::default(), Severity::Info);
    }

    #[test]
    fn status_style_maps_common_netbox_statuses() {
        let t = Theme::default_theme();
        // Healthy → success (green).
        assert_eq!(t.status_style("active").fg, Some(t.success));
        // In-progress → warning.
        assert_eq!(t.status_style("planned").fg, Some(t.warning));
        assert_eq!(t.status_style("staged").fg, Some(t.warning));
        assert_eq!(t.status_style("provisioning").fg, Some(t.warning));
        // Out-of-service / failed → error.
        assert_eq!(t.status_style("offline").fg, Some(t.error));
        assert_eq!(t.status_style("deprecated").fg, Some(t.error));
        assert_eq!(t.status_style("failed").fg, Some(t.error));
        assert_eq!(t.status_style("decommissioning").fg, Some(t.error));
        // Unknown / other → neutral text (no alarm color).
        assert_eq!(t.status_style("whatever").fg, Some(t.text));
    }

    #[test]
    fn status_style_is_case_and_whitespace_insensitive() {
        let t = Theme::default_theme();
        assert_eq!(t.status_style("ACTIVE").fg, t.status_style("active").fg);
        assert_eq!(
            t.status_style("  Offline ").fg,
            t.status_style("offline").fg
        );
    }

    #[test]
    fn no_color_theme_is_flagged_and_all_fields_reset() {
        let t = Theme::no_color();
        assert!(t.is_no_color());
        assert_eq!(t.name(), "no_color");
        // Every themed color field is Reset (no color emitted at all).
        for color in [
            t.border,
            t.border_focused,
            t.text,
            t.text_dim,
            t.highlight_bg,
            t.chrome_bg,
            t.success,
            t.warning,
            t.error,
            t.accent,
            t.header,
            t.graph_primary,
            t.graph_secondary,
        ] {
            assert_eq!(color, Color::Reset);
        }
    }

    #[test]
    fn no_color_theme_message_style_is_unstyled_for_every_severity() {
        let t = Theme::no_color();
        for sev in [
            Severity::Info,
            Severity::Success,
            Severity::Warning,
            Severity::Error,
        ] {
            // No foreground color set: a bare default style, regardless of severity.
            assert_eq!(t.message_style(sev), Style::default());
            assert_eq!(t.message_style(sev).fg, None);
        }
    }

    #[test]
    fn no_color_theme_status_style_is_unstyled() {
        let t = Theme::no_color();
        // Statuses that would otherwise color (active→green, offline→red) stay bare.
        assert_eq!(t.status_style("active"), Style::default());
        assert_eq!(t.status_style("offline"), Style::default());
        assert_eq!(t.status_style("whatever").fg, None);
    }

    #[test]
    fn by_name_light_and_alias() {
        assert_eq!(Theme::by_name("light").name(), "light");
        // The `day` alias resolves to the same light theme.
        assert_eq!(Theme::by_name("day").name(), "light");
        assert_eq!(Theme::by_name("DAY").name(), "light");
    }

    #[test]
    fn light_is_listed_and_cycled() {
        assert!(Theme::list().contains(&"light"));
        // Present in the cycle list means `t` (which iterates list()) reaches it.
        assert_eq!(
            Theme::by_name(Theme::list()[Theme::index_of("light")]).name(),
            "light"
        );
    }

    #[test]
    fn light_is_a_real_color_theme_not_no_color() {
        let t = Theme::light();
        assert!(!t.is_no_color());
        // It is a genuine light-background palette: its selection highlight is a
        // light tint, distinct from the default (dark) theme's DarkGray highlight,
        // and its dark ink text differs from the default theme's white text.
        let dflt = Theme::default_theme();
        assert_ne!(t.highlight_bg, dflt.highlight_bg);
        assert_ne!(t.text, dflt.text);
        // Sanity: the highlight background is a light color (high RGB), so dark
        // foreground text stays legible against it.
        if let Color::Rgb(r, g, b) = t.highlight_bg {
            assert!(
                r > 180 && g > 180 && b > 180,
                "light highlight_bg should be light"
            );
        } else {
            panic!("light theme highlight_bg should be an explicit RGB color");
        }
    }

    #[test]
    fn ordinary_themes_are_not_no_color() {
        // The named `monochrome` theme is grayscale RGB — still color, not no_color.
        assert!(!Theme::default_theme().is_no_color());
        assert!(!Theme::monochrome().is_no_color());
        for name in Theme::list() {
            assert!(!Theme::by_name(name).is_no_color());
        }
    }
}
