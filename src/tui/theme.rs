//! Color theme definitions for the TUI.
//!
//! Provides 11 built-in themes: default, kawaii, cyber, dracula, monochrome,
//! matrix, nord, gruvbox, catppuccin, tokyo_night, solarized. The active theme
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

    // UI chrome
    pub border: Color,
    pub border_focused: Color,
    pub text: Color,
    pub text_dim: Color,
    pub highlight_bg: Color,

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

            border: Color::Cyan,
            border_focused: Color::Cyan,
            text: Color::White,
            text_dim: Color::Gray,
            highlight_bg: Color::DarkGray,

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

            border: Color::Rgb(255, 182, 214),
            border_focused: Color::Rgb(255, 182, 214),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(180, 180, 200),
            highlight_bg: Color::Rgb(60, 50, 70),

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

            border: Color::Rgb(0, 255, 255),
            border_focused: Color::Rgb(0, 255, 255),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(100, 100, 120),
            highlight_bg: Color::Rgb(20, 20, 35),

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

            border: Color::Rgb(189, 147, 249),
            border_focused: Color::Rgb(189, 147, 249),
            text: Color::Rgb(248, 248, 242),
            text_dim: Color::Rgb(98, 114, 164),
            highlight_bg: Color::Rgb(68, 71, 90),

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

            border: Color::Rgb(200, 200, 200),
            border_focused: Color::Rgb(200, 200, 200),
            text: Color::Rgb(255, 255, 255),
            text_dim: Color::Rgb(120, 120, 120),
            highlight_bg: Color::Rgb(50, 50, 50),

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

            border: Color::Rgb(0, 255, 0),
            border_focused: Color::Rgb(0, 255, 0),
            text: Color::Rgb(0, 255, 0),
            text_dim: Color::Rgb(0, 100, 0),
            highlight_bg: Color::Rgb(0, 20, 0),

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

            border: Color::Rgb(136, 192, 208),
            border_focused: Color::Rgb(136, 192, 208),
            text: Color::Rgb(236, 239, 244),
            text_dim: Color::Rgb(76, 86, 106),
            highlight_bg: Color::Rgb(59, 66, 82),

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

            border: Color::Rgb(254, 128, 25),
            border_focused: Color::Rgb(254, 128, 25),
            text: Color::Rgb(235, 219, 178),
            text_dim: Color::Rgb(146, 131, 116),
            highlight_bg: Color::Rgb(80, 73, 69),

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

            border: Color::Rgb(203, 166, 247),
            border_focused: Color::Rgb(203, 166, 247),
            text: Color::Rgb(205, 214, 244),
            text_dim: Color::Rgb(108, 112, 134),
            highlight_bg: Color::Rgb(88, 91, 112),

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

            border: Color::Rgb(187, 154, 247),
            border_focused: Color::Rgb(187, 154, 247),
            text: Color::Rgb(192, 202, 245),
            text_dim: Color::Rgb(86, 95, 137),
            highlight_bg: Color::Rgb(59, 66, 97),

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

            border: Color::Rgb(42, 161, 152),
            border_focused: Color::Rgb(42, 161, 152),
            text: Color::Rgb(131, 148, 150),
            text_dim: Color::Rgb(88, 110, 117),
            highlight_bg: Color::Rgb(7, 54, 66),

            success: Color::Rgb(133, 153, 0),
            warning: Color::Rgb(181, 137, 0),
            error: Color::Rgb(220, 50, 47),

            accent: Color::Rgb(181, 137, 0),
            header: Color::Rgb(203, 75, 22),

            graph_primary: Color::Rgb(133, 153, 0),
            graph_secondary: Color::Rgb(42, 161, 152),
        }
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
}
