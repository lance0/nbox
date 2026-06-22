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
use ratatui::widgets::Paragraph;
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
    /// When set, the rendered text is masked with `•` (password mode): the raw
    /// value is never drawn. The editing model and [`value`](Self::value) are
    /// unchanged — only the on-screen glyphs differ — so a token field can be
    /// edited normally while never exposing its characters. Drives the masked
    /// branch of [`rendered_text`](Self::rendered_text) and cheese's
    /// `password_mode` at render time.
    masked: bool,
}

/// The glyph a masked input renders for each character (a bullet, never the raw
/// value). Public so a form's render path / tests can name the mask without
/// reaching into a cheese type.
pub const MASK_CHAR: char = '•';

impl TextInput {
    /// A fresh, empty input with the given placeholder (shown when empty).
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            state: InputState::new(),
            placeholder: placeholder.into(),
            masked: false,
        }
    }

    /// A fresh, empty *masked* input (password mode): its value renders as a row
    /// of [`MASK_CHAR`] bullets, never the raw text. Used for the token field.
    pub fn masked(placeholder: impl Into<String>) -> Self {
        Self {
            state: InputState::new(),
            placeholder: placeholder.into(),
            masked: true,
        }
    }

    /// Whether this input masks its value on screen.
    pub fn is_masked(&self) -> bool {
        self.masked
    }

    /// The text as it appears on screen: the raw value, or — when
    /// [`masked`](Self::masked) — one [`MASK_CHAR`] per character, so the secret
    /// is never exposed in any rendered string. Pure; the editing buffer and
    /// [`value`](Self::value) are untouched. Tests assert a masked field's
    /// rendered text contains no character of the real value.
    pub fn rendered_text(&self) -> String {
        if self.masked {
            MASK_CHAR
                .to_string()
                .repeat(self.state.value().chars().count())
        } else {
            self.state.value().to_string()
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
            // Cursor moves are consumed but are NOT edits: they don't change the
            // value, so they must return `false` (M12) — a `true` here would make
            // the caller needlessly refilter a search or invalidate a test result.
            KeyCode::Left => {
                self.state.move_left();
                false
            }
            KeyCode::Right => {
                self.state.move_right();
                false
            }
            KeyCode::Home => {
                self.state.home();
                false
            }
            KeyCode::End => {
                self.state.end();
                false
            }
            // Enter, Esc, and anything else aren't edits — the caller decides.
            _ => false,
        }
    }

    /// The current text.
    pub fn value(&self) -> &str {
        self.state.value()
    }

    /// Replace the buffer with `value`, cursor at the end. Used to prefill a
    /// field when editing an existing record. PURE: no I/O.
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.state.set_value(value.into());
        self.state.end();
    }

    /// The cursor position (in chars from the start), for placing the terminal
    /// cursor on a focused field.
    pub fn cursor_pos(&self) -> usize {
        self.state.cursor_pos()
    }

    /// The cursor's offset in *display columns* from the start of the value (wide
    /// CJK glyphs count as 2; masked chars are 1 wide each). Used by a prompt-less
    /// field (the [`FormInput`] rows) to place the terminal cursor: the text
    /// starts at the value area's left edge, so the cursor sits this many columns
    /// in. PURE; no I/O.
    pub fn cursor_display_col(&self) -> u16 {
        self.value()
            .chars()
            .take(self.state.cursor_pos())
            .map(|c| char_width(c) as u16)
            .sum()
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
        self.render_with_focus(frame, area, sigil, theme, true)
    }

    /// Like [`render`](Self::render) but with explicit focus: a focused field draws
    /// cheese's cursor cell, an unfocused one doesn't (so a multi-input screen — the
    /// Settings form — shows a single cursor). The returned position is still the
    /// focused cursor cell, for the caller to place a real terminal cursor.
    pub fn render_with_focus(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        sigil: char,
        theme: &Theme,
        focused: bool,
    ) -> Position {
        // Focus is render-time UI state, not part of the pure editing model.
        self.state.set_focused(focused);

        let palette = cheese_palette(theme);
        let sigil = sigil.to_string();
        let input = Input::new("")
            .prompt(&sigil)
            .placeholder(&self.placeholder)
            .password_mode(self.masked)
            .password_char(MASK_CHAR)
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

/// A labelled multi-field text form — an ordered set of [`TextInput`]s plus a
/// focus index — built on the same pure, cheese-backed editing model.
///
/// Keystrokes route to the focused field; `Tab`/`Shift-Tab` move focus (wrapping
/// at both ends). One field can be [`masked`](TextInput::masked) (the token
/// field), so its value renders as bullets and never appears in any rendered
/// string. PURE + unit-testable: [`focus_next`](Self::focus_next)/
/// [`focus_prev`](Self::focus_prev)/[`handle_key`](Self::handle_key) are state
/// transitions with no I/O, and [`rendered_lines`](Self::rendered_lines) returns
/// the label+value strings the renderer paints (so masking is testable).
///
/// The form holds *only* free-text fields; non-text controls (an auth-scheme
/// cycle, a verify-tls toggle) are owned by the caller's modal state, not here.
pub struct FormInput {
    /// `(label, input)` in display order.
    fields: Vec<(String, TextInput)>,
    /// Index of the focused field; always in range while `fields` is non-empty.
    focus: usize,
}

impl FormInput {
    /// Build a form from `(label, input)` pairs, focusing the first field.
    pub fn new(fields: Vec<(String, TextInput)>) -> Self {
        Self { fields, focus: 0 }
    }

    /// The index of the currently focused field.
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// Number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Whether the form has no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Move focus to the next field, wrapping past the last back to the first.
    /// No-op on an empty form.
    pub fn focus_next(&mut self) {
        if !self.fields.is_empty() {
            self.focus = (self.focus + 1) % self.fields.len();
        }
    }

    /// Move focus to the previous field, wrapping before the first to the last.
    /// No-op on an empty form.
    pub fn focus_prev(&mut self) {
        if !self.fields.is_empty() {
            let len = self.fields.len();
            self.focus = (self.focus + len - 1) % len;
        }
    }

    /// Route a key to the focused field's editor. `Tab`/`Shift-Tab` move focus
    /// instead of editing. Returns `true` when the key was consumed (an edit or a
    /// focus move), `false` for a pass-through (Enter/Esc, left to the caller).
    /// PURE: no I/O.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Tab => {
                self.focus_next();
                true
            }
            KeyCode::BackTab => {
                self.focus_prev();
                true
            }
            _ => match self.fields.get_mut(self.focus) {
                Some((_, input)) => input.handle_key(key),
                None => false,
            },
        }
    }

    /// The current value of the field at `idx`, if any. The raw value — callers
    /// reading the token to write to config use this; it is never rendered.
    pub fn value(&self, idx: usize) -> Option<&str> {
        self.fields.get(idx).map(|(_, i)| i.value())
    }

    /// Mutable access to the input at `idx`, for prefilling on edit.
    pub fn input_mut(&mut self, idx: usize) -> Option<&mut TextInput> {
        self.fields.get_mut(idx).map(|(_, i)| i)
    }

    /// The label + on-screen text for each field, in order, as the renderer would
    /// paint them: a masked field's text is bullets, never the secret. Pure, so a
    /// test can assert the token field's rendered string exposes no real char.
    pub fn rendered_lines(&self) -> Vec<(String, String)> {
        self.fields
            .iter()
            .map(|(label, input)| (label.clone(), input.rendered_text()))
            .collect()
    }

    /// Render the form into `area`, one labelled row per field, returning the
    /// terminal cursor position for the focused field so the caller can place a
    /// real cursor there. Styling comes from [`cheese_palette`] (Theme is the
    /// source of truth). A masked field draws bullets via the field's own
    /// password mode.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) -> Option<Position> {
        let palette = cheese_palette(theme);
        let label_w = self
            .fields
            .iter()
            .map(|(l, _)| display_width(l))
            .max()
            .unwrap_or(0);
        let mut cursor = None;
        for (row, (label, input)) in self.fields.iter_mut().enumerate() {
            let y = area.y.saturating_add(row as u16);
            if y >= area.bottom() {
                break;
            }
            // Label cell (right-padded) in the muted/secondary color.
            let label_cell = format!("{label:<label_w$}  ");
            let label_w16 = display_width(&label_cell) as u16;
            frame.render_widget(
                Paragraph::new(Span::styled(
                    label_cell.clone(),
                    Style::default().fg(palette.secondary),
                )),
                Rect::new(area.x, y, label_w16.min(area.width), 1),
            );
            // The value sits after the label cell.
            let value_x = area.x.saturating_add(label_w16);
            let value_w = area.width.saturating_sub(label_w16);
            if value_w == 0 {
                continue;
            }
            let value_area = Rect::new(value_x, y, value_w, 1);
            let focused = row == self.focus;
            input.state.set_focused(focused);
            let widget = Input::new("")
                // cheese's `Input` defaults its prompt to ">"; a form field has no
                // sigil, so set an explicit empty prompt or every field renders a
                // stray "> " and the cursor lands two cells off.
                .prompt("")
                .placeholder(&input.placeholder)
                .password_mode(input.masked)
                .password_char(MASK_CHAR)
                .palette(&palette);
            frame.render_stateful_widget(widget, value_area, &mut input.state);
            if focused {
                // With an empty prompt the text starts at the value area's left
                // edge, so the cursor sits `cursor_pos` display columns into it.
                let x = value_x
                    .saturating_add(input.cursor_display_col())
                    .min(value_area.right().saturating_sub(1));
                cursor = Some(Position::new(x, y));
            }
        }
        cursor
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
    fn cursor_moves_are_consumed_but_not_reported_as_edits() {
        // M12: Left/Right/Home/End move the cursor without changing the value, so
        // they must return false — the caller shouldn't refilter / invalidate a
        // test on a bare cursor move. Edits (chars, backspace, delete) return true.
        let mut input = TextInput::new("");
        type_str(&mut input, "abc");
        assert!(!input.handle_key(key(KeyCode::Left)), "Left is not an edit");
        assert!(
            !input.handle_key(key(KeyCode::Right)),
            "Right is not an edit"
        );
        assert!(!input.handle_key(key(KeyCode::Home)), "Home is not an edit");
        assert!(!input.handle_key(key(KeyCode::End)), "End is not an edit");
        // The value is unchanged, and a real edit still reports true.
        assert_eq!(input.value(), "abc");
        assert!(input.handle_key(key(KeyCode::Char('d'))));
        assert!(input.handle_key(key(KeyCode::Backspace)));
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

    fn form(labels: &[&str]) -> FormInput {
        let fields = labels
            .iter()
            .map(|l| ((*l).to_string(), TextInput::new("")))
            .collect();
        FormInput::new(fields)
    }

    #[test]
    fn form_focus_next_and_prev_wrap() {
        let mut f = form(&["name", "url", "token_env"]);
        assert_eq!(f.focus(), 0);
        f.focus_next();
        assert_eq!(f.focus(), 1);
        f.focus_next();
        assert_eq!(f.focus(), 2);
        // Next past the last wraps to the first.
        f.focus_next();
        assert_eq!(f.focus(), 0);
        // Prev before the first wraps to the last.
        f.focus_prev();
        assert_eq!(f.focus(), 2);
        f.focus_prev();
        assert_eq!(f.focus(), 1);
    }

    #[test]
    fn form_tab_and_backtab_move_focus_and_are_consumed() {
        let mut f = form(&["a", "b"]);
        assert!(f.handle_key(key(KeyCode::Tab)));
        assert_eq!(f.focus(), 1);
        assert!(f.handle_key(key(KeyCode::BackTab)));
        assert_eq!(f.focus(), 0);
    }

    #[test]
    fn form_routes_keystrokes_to_the_focused_field_only() {
        let mut f = form(&["name", "url"]);
        // Type into field 0.
        for c in "edge".chars() {
            assert!(f.handle_key(key(KeyCode::Char(c))));
        }
        assert_eq!(f.value(0), Some("edge"));
        assert_eq!(f.value(1), Some(""), "the unfocused field is untouched");
        // Move focus and type into field 1.
        f.handle_key(key(KeyCode::Tab));
        for c in "https://x".chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(f.value(0), Some("edge"), "field 0 keeps its value");
        assert_eq!(f.value(1), Some("https://x"));
    }

    #[test]
    fn form_passes_through_enter_and_esc() {
        let mut f = form(&["a"]);
        // Non-editing keys are not consumed — the caller handles submit/cancel.
        assert!(!f.handle_key(key(KeyCode::Enter)));
        assert!(!f.handle_key(key(KeyCode::Esc)));
    }

    #[test]
    fn masked_field_never_exposes_its_value_in_the_rendered_string() {
        let mut token = TextInput::masked("token");
        type_str(&mut token, "nbt_secret123");
        // The editing value is intact (the config save reads it)…
        assert_eq!(token.value(), "nbt_secret123");
        // …but the rendered string is all bullets, exposing no real character.
        let shown = token.rendered_text();
        assert!(shown.chars().all(|c| c == super::MASK_CHAR));
        assert_eq!(shown.chars().count(), "nbt_secret123".chars().count());
        for c in "nbt_secret123".chars() {
            assert!(
                !shown.contains(c) || c == super::MASK_CHAR,
                "rendered text must not contain a real value char ({c})"
            );
        }
        // A non-masked field renders verbatim.
        let mut name = TextInput::new("name");
        type_str(&mut name, "edge01");
        assert_eq!(name.rendered_text(), "edge01");
    }

    #[test]
    fn form_rendered_lines_mask_only_the_masked_field() {
        let fields = vec![
            ("name".to_string(), TextInput::new("")),
            ("token".to_string(), TextInput::masked("")),
        ];
        let mut f = FormInput::new(fields);
        // name = "edge01"
        for c in "edge01".chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
        // focus token, type a secret
        f.handle_key(key(KeyCode::Tab));
        for c in "s3cr3t".chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
        let lines = f.rendered_lines();
        assert_eq!(lines[0], ("name".to_string(), "edge01".to_string()));
        assert_eq!(lines[1].0, "token");
        assert!(lines[1].1.chars().all(|c| c == super::MASK_CHAR));
        assert!(!lines[1].1.contains("s3cr3t"));
    }

    #[test]
    fn form_set_value_prefills_for_edit() {
        let mut f = form(&["name", "url"]);
        f.input_mut(0).unwrap().set_value("core01");
        f.input_mut(1).unwrap().set_value("https://nb");
        assert_eq!(f.value(0), Some("core01"));
        assert_eq!(f.value(1), Some("https://nb"));
        // Cursor lands at the end so further typing appends.
        for c in "/api".chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(f.value(0), Some("core01/api"));
    }

    #[test]
    fn cursor_display_col_is_zero_based_and_width_aware() {
        // A prompt-less FormInput field places its cursor `cursor_display_col`
        // columns into the value area — starting at 0 (no stray prompt offset).
        let mut input = TextInput::new("");
        // Empty field: cursor at the very start (column 0), not offset by a prompt.
        assert_eq!(input.cursor_display_col(), 0);
        type_str(&mut input, "abc");
        // After "abc" the cursor is 3 columns in.
        assert_eq!(input.cursor_display_col(), 3);
        input.handle_key(key(KeyCode::Home));
        assert_eq!(input.cursor_display_col(), 0);
        // A wide (CJK) glyph counts as two display columns.
        let mut wide = TextInput::new("");
        type_str(&mut wide, "世a"); // 世 is width 2, a is width 1
        assert_eq!(wide.cursor_display_col(), 3);
        wide.handle_key(key(KeyCode::Left)); // cursor before 'a', after 世
        assert_eq!(wide.cursor_display_col(), 2);
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
