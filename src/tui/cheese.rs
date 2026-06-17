//! Adapter layer over the `ratatui-cheese` widget crate.
//!
//! All `ratatui_cheese::*` types are confined to this module. The rest of nbox
//! talks to the thin wrappers here ([`TextInput`], [`Spinner`]) and never names a
//! cheese type directly — so cheese stays out of `AppCommand`, the domain views,
//! and any NetBox-facing state. The TUI may hold a [`TextInput`]/[`Spinner`];
//! those are the only seams. (The help overlay is hand-rolled in `tui::ui`, not
//! a cheese widget.)
//!
//! [`Theme`](crate::tui::theme::Theme) remains the single source of truth for
//! colors. [`cheese_palette`] maps nbox's `Theme` onto cheese's `Palette`; we
//! never read cheese's built-in presets.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui_cheese::input::{Input, InputState};
use ratatui_cheese::spinner::{SpinnerState, SpinnerType};
use ratatui_cheese::theme::Palette;

use crate::tui::theme::Theme;

/// Map nbox's [`Theme`] onto cheese's `Palette`. `Theme` is the source of truth;
/// this is the one place the field-by-field translation lives.
///
/// cheese's palette has roles nbox's theme doesn't model 1:1 (`muted`/`faint`
/// shades of text, a raised `surface`, an `on_highlight` text color). We map
/// each cheese role onto the nearest nbox color: the two dim shades both fall
/// back to `text_dim`, `surface` to the highlight background, and `on_highlight`
/// to the normal text so highlighted text stays legible against `highlight_bg`.
pub fn cheese_palette(theme: &Theme) -> Palette {
    Palette {
        foreground: theme.text,
        muted: theme.text_dim,
        faint: theme.text_dim,
        primary: theme.accent,
        secondary: theme.header,
        surface: theme.highlight_bg,
        border: theme.border,
        highlight: theme.highlight_bg,
        on_highlight: theme.text,
        error: theme.error,
        success: theme.success,
    }
}

/// A loading spinner — nbox's thin newtype around cheese's `SpinnerState`.
///
/// The footer shows it next to the status message only while a request is in
/// flight (see `App::loading`); it stops advancing — and isn't drawn — when
/// idle, so there's no busy-spin at rest. [`tick`](Spinner::tick) is a pure
/// frame advance (no I/O, no wall-clock read), so the loading→glyph cycle can be
/// driven and unit-tested through the pure `handle_event` seam. Styling comes
/// from [`cheese_palette`] so [`Theme`] stays the source of truth.
pub struct Spinner {
    state: SpinnerState,
}

impl Spinner {
    /// A fresh spinner at frame 0, using the braille mini-dot preset (a compact
    /// single-cell glyph that reads well inline in the status line).
    pub fn new() -> Self {
        Self {
            state: SpinnerState::new(SpinnerType::MiniDot),
        }
    }

    /// Advance exactly one frame. PURE: no wall-clock read — we feed cheese the
    /// preset's own frame interval so each call steps the animation by a single
    /// glyph, deterministically. Call this on a tick *only while loading* so the
    /// spinner is still when nothing is in flight.
    pub fn tick(&mut self) {
        let interval = self.state.interval();
        self.state.tick(interval);
    }

    /// Reset to the first frame. Called when loading ends so the next request
    /// starts the animation from a clean glyph rather than mid-cycle.
    pub fn reset(&mut self) {
        self.state = SpinnerState::new(SpinnerType::MiniDot);
    }

    /// The current frame's glyph (e.g. `⠋`). Used to measure/inspect the spinner
    /// without pulling a cheese type out of this module.
    pub fn frame(&self) -> &str {
        self.state.frame_str()
    }

    /// The current glyph as a styled ratatui [`Span`], colored via
    /// [`cheese_palette`] (`primary`/accent role) so the footer can render it
    /// inline without ever naming a cheese type. [`Theme`] is the color source.
    pub fn span(&self, theme: &Theme) -> Span<'static> {
        let palette = cheese_palette(theme);
        Span::styled(
            self.frame().to_string(),
            Style::default().fg(palette.primary),
        )
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

/// A single-line text input — nbox's thin newtype around cheese's `InputState`.
///
/// Editing is delegated to cheese: character entry, backspace/delete, cursor
/// movement (left/right/home/end), and a visible cursor. [`handle_key`] is a
/// pure state transition (no I/O), so it can drive the pure `handle_event` seam
/// and be unit-tested without a terminal.
///
/// [`handle_key`]: TextInput::handle_key
pub struct TextInput {
    state: InputState,
    placeholder: String,
}

impl TextInput {
    /// A fresh, empty input with the given placeholder (shown when empty).
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            state: InputState::new(),
            placeholder: placeholder.into(),
        }
    }

    /// Apply a key to the input, mutating its text/cursor. PURE: no I/O, just a
    /// state transition over the editable buffer — safe to call from the pure
    /// `handle_event`. Returns `true` if the key was consumed as an edit (so the
    /// caller can refilter), `false` if it isn't an editing key (Enter/Esc and
    /// other control keys are left for the caller to handle).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Ctrl+U: clear the whole line. Ctrl+W: delete the previous word.
            KeyCode::Char('u') if ctrl => {
                self.clear();
                true
            }
            KeyCode::Char('w') if ctrl => {
                self.delete_prev_word();
                true
            }
            // A bare character (no Ctrl) is text entry.
            KeyCode::Char(c) if !ctrl => {
                self.state.insert_char(c);
                true
            }
            KeyCode::Backspace => {
                self.state.delete_before();
                true
            }
            KeyCode::Delete => {
                self.state.delete_at();
                true
            }
            KeyCode::Left => {
                self.state.move_left();
                true
            }
            KeyCode::Right => {
                self.state.move_right();
                true
            }
            KeyCode::Home => {
                self.state.home();
                true
            }
            KeyCode::End => {
                self.state.end();
                true
            }
            // Enter, Esc, and anything else aren't edits — the caller decides.
            _ => false,
        }
    }

    /// The current text.
    pub fn value(&self) -> &str {
        self.state.value()
    }

    /// Reset to empty, cursor at the start.
    pub fn clear(&mut self) {
        self.state = InputState::new();
    }

    /// Alias for [`clear`](Self::clear) — reset the buffer to empty.
    pub fn reset(&mut self) {
        self.clear();
    }

    /// Delete the word before the cursor (and the spaces before it), like the
    /// previous `Ctrl+W` behavior — implemented over the cheese cursor API so
    /// the buffer and cursor stay consistent. No-op at the start of the line.
    fn delete_prev_word(&mut self) {
        // Walk back over any spaces, then over the word, deleting before the
        // cursor each step. `delete_before` pulls the char left of the cursor
        // and steps the cursor back, so checking the new char-left each time
        // tracks the word boundary without indexing into the buffer.
        while self.char_before_cursor() == Some(' ') {
            self.state.delete_before();
        }
        while matches!(self.char_before_cursor(), Some(c) if c != ' ') {
            self.state.delete_before();
        }
    }

    /// The character immediately to the left of the cursor, if any.
    fn char_before_cursor(&self) -> Option<char> {
        let pos = self.state.cursor_pos();
        if pos == 0 {
            return None;
        }
        self.state.value().chars().nth(pos - 1)
    }

    /// Render the input into `area` with a leading `sigil` (e.g. `/` or `:`) and
    /// the theme's colors, and return the terminal cursor position so the caller
    /// can place a real cursor there. Drawn as a single line: `sigil value`.
    ///
    /// Styling comes from [`cheese_palette`] so [`Theme`] stays the source of
    /// truth. The returned `(x, y)` is the cell the next character would land in;
    /// the caller passes it to `Frame::set_cursor_position`.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        sigil: char,
        theme: &Theme,
    ) -> Position {
        // Focus so cheese draws its cursor cell; this is render-time UI state,
        // not part of the pure editing model.
        self.state.set_focused(true);

        let palette = cheese_palette(theme);
        let sigil = sigil.to_string();
        let input = Input::new("")
            .prompt(&sigil)
            .placeholder(&self.placeholder)
            .palette(&palette);
        frame.render_stateful_widget(input, area, &mut self.state);

        self.cursor_position(area, &sigil)
    }

    /// Compute the terminal cursor cell, mirroring cheese's render math: the
    /// prompt (`sigil` + a trailing space) sits at `area.x`, the text starts
    /// after it, and the cursor sits `cursor_pos` display columns into the text.
    ///
    /// The footer is full terminal width and these inputs are short, so the
    /// renderer's horizontal scroll never kicks in (`scroll_offset` stays 0) —
    /// the cell is simply `prompt_width + display width of the text before the
    /// cursor`, clamped to the area.
    fn cursor_position(&self, area: Rect, sigil: &str) -> Position {
        // cheese builds the prompt as `"{prompt} "` — sigil plus one space.
        let prompt_width = display_width(sigil) + 1;
        let before: usize = self
            .value()
            .chars()
            .take(self.state.cursor_pos())
            .map(char_width)
            .sum();
        let x = area
            .x
            .saturating_add((prompt_width + before) as u16)
            .min(area.right().saturating_sub(1));
        Position::new(x, area.y)
    }
}

/// Display width of a single char (wide CJK glyphs count as 2), matching the
/// width cheese uses to lay out the cursor. Control chars count as 0.
fn char_width(ch: char) -> usize {
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Display width of a string — the sum of its chars' widths.
fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_str(input: &mut TextInput, s: &str) {
        for c in s.chars() {
            input.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_builds_the_value() {
        let mut input = TextInput::new("");
        type_str(&mut input, "edge01");
        assert_eq!(input.value(), "edge01");
    }

    #[test]
    fn handle_key_reports_edits_vs_passthrough() {
        let mut input = TextInput::new("");
        // A character is consumed as an edit.
        assert!(input.handle_key(key(KeyCode::Char('a'))));
        // Enter and Esc are NOT edits — the caller handles submit/cancel.
        assert!(!input.handle_key(key(KeyCode::Enter)));
        assert!(!input.handle_key(key(KeyCode::Esc)));
        // The buffer is untouched by the pass-through keys.
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn backspace_deletes_before_the_cursor() {
        let mut input = TextInput::new("");
        type_str(&mut input, "edge");
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.value(), "edg");
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut input = TextInput::new("");
        type_str(&mut input, "edge");
        input.handle_key(key(KeyCode::Home)); // cursor at start
        input.handle_key(key(KeyCode::Delete)); // delete 'e'
        assert_eq!(input.value(), "dge");
    }

    #[test]
    fn left_right_move_the_insertion_point() {
        let mut input = TextInput::new("");
        type_str(&mut input, "ace");
        // Move left once (cursor between 'c' and 'e') and insert 'd' → "aced"? no:
        // "ace", cursor at end (3). Left → 2 (before 'e'); insert 'd' → "acde".
        input.handle_key(key(KeyCode::Left));
        type_str(&mut input, "d");
        assert_eq!(input.value(), "acde");
        // Right moves past 'e'; inserting at end appends.
        input.handle_key(key(KeyCode::Right));
        type_str(&mut input, "f");
        assert_eq!(input.value(), "acdef");
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        let mut input = TextInput::new("");
        type_str(&mut input, "mid");
        input.handle_key(key(KeyCode::Home));
        type_str(&mut input, "X"); // prepend
        assert_eq!(input.value(), "Xmid");
        input.handle_key(key(KeyCode::End));
        type_str(&mut input, "Y"); // append
        assert_eq!(input.value(), "XmidY");
    }

    #[test]
    fn ctrl_u_clears_the_line() {
        let mut input = TextInput::new("");
        type_str(&mut input, "edge router");
        assert!(input.handle_key(ctrl('u')));
        assert_eq!(input.value(), "");
    }

    #[test]
    fn ctrl_w_deletes_the_previous_word() {
        let mut input = TextInput::new("");
        type_str(&mut input, "edge router");
        input.handle_key(ctrl('w'));
        // Word + nothing else after it: deletes "router", keeps the trailing
        // space removed too (matching the old trim-last-word behavior).
        assert_eq!(input.value(), "edge ");
        // A second Ctrl+W eats the remaining word and its space.
        input.handle_key(ctrl('w'));
        assert_eq!(input.value(), "");
    }

    #[test]
    fn ctrl_w_at_start_is_a_noop() {
        let mut input = TextInput::new("");
        input.handle_key(ctrl('w'));
        assert_eq!(input.value(), "");
    }

    #[test]
    fn clear_and_reset_empty_the_buffer() {
        let mut input = TextInput::new("");
        type_str(&mut input, "abc");
        input.clear();
        assert_eq!(input.value(), "");
        type_str(&mut input, "def");
        input.reset();
        assert_eq!(input.value(), "");
    }

    #[test]
    fn spinner_tick_advances_one_frame_and_cycles() {
        let mut s = Spinner::new();
        let first = s.frame().to_string();
        s.tick();
        let second = s.frame().to_string();
        // One tick steps the animation by exactly one glyph.
        assert_ne!(first, second, "tick must advance the frame");
        // Walking the full preset cycles back to the first glyph.
        let total = ratatui_cheese::spinner::SpinnerType::MiniDot.frames().len();
        let mut seen = vec![first.clone(), second];
        for _ in 2..total {
            s.tick();
            seen.push(s.frame().to_string());
        }
        s.tick(); // one more wraps around
        assert_eq!(s.frame(), first, "the glyph sequence wraps");
        // The frames within a cycle are not all identical (it actually animates).
        assert!(seen.iter().any(|g| *g != first));
    }

    #[test]
    fn spinner_reset_returns_to_first_frame() {
        let mut s = Spinner::new();
        let first = s.frame().to_string();
        s.tick();
        s.tick();
        assert_ne!(s.frame(), first);
        s.reset();
        assert_eq!(s.frame(), first, "reset returns to frame 0");
    }

    #[test]
    fn spinner_span_uses_the_theme_accent_and_current_glyph() {
        let theme = Theme::default_theme();
        let mut s = Spinner::new();
        let span = s.span(&theme);
        // The glyph matches the current frame and it's colored by the palette's
        // primary (accent) role, keeping Theme the source of truth.
        assert_eq!(span.content, s.frame());
        assert_eq!(span.style.fg, Some(cheese_palette(&theme).primary));
        // After a tick the span tracks the new glyph.
        s.tick();
        assert_eq!(s.span(&theme).content, s.frame());
    }

    #[test]
    fn cursor_position_tracks_text_before_cursor() {
        let mut input = TextInput::new("");
        type_str(&mut input, "abc");
        let area = Rect::new(0, 5, 40, 1);
        // sigil "/" → prompt "/ " width 2; cursor after "abc" → 2 + 3 = 5.
        let pos = input.cursor_position(area, "/");
        assert_eq!(pos, Position::new(5, 5));
        // Move home → cursor sits right after the prompt at x = 2.
        input.handle_key(key(KeyCode::Home));
        assert_eq!(input.cursor_position(area, "/"), Position::new(2, 5));
    }
}
