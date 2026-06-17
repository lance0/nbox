//! TUI rendering.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::tui::state::{App, Focus, Mode, Screen, result_row_cells};
use crate::tui::theme::Theme;

/// Column widths for the results table. KIND is fixed-width so the kind tags line
/// up; DISPLAY flexes to fill the middle; SITE gets a fixed tail column. These
/// give ratatui the [`Constraint`]s it needs to align the cells into columns.
const KIND_COL: u16 = 8;
const SITE_COL: u16 = 14;

/// Render the whole UI for the current frame.
pub fn render(frame: &mut Frame, app: &mut App) {
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let header = {
        let theme = &app.theme;
        Line::from(vec![
            Span::styled(
                format!(" profile: {} ", app.profile_name),
                Style::default().fg(theme.header),
            ),
            Span::styled(
                format!("netbox: {} (v{}) ", app.base_url, app.netbox_version),
                Style::default().fg(theme.text_dim),
            ),
            Span::styled(
                format!("mode: {} ", mode_label(app.mode)),
                Style::default().fg(theme.accent),
            ),
        ])
    };
    frame.render_widget(Paragraph::new(header), areas[0]);

    match app.screen {
        Screen::Home => render_home(frame, areas[1], app),
        Screen::Help => render_help(frame, areas[1], app),
        Screen::Detail => render_detail(frame, areas[1], app),
    }

    render_footer(frame, areas[2], app);
}

fn render_home(frame: &mut Frame, area: Rect, app: &mut App) {
    // Split the body: list on the left (~40%), live preview on the right (~60%).
    let panes =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(area);
    render_home_list(frame, panes[0], app);
    render_home_preview(frame, panes[1], app);
}

/// Border style for a pane given whether it currently holds focus: the focused
/// pane gets the theme's focused-border color, the other a dim border.
fn pane_border(theme: &Theme, focused: bool) -> Style {
    if focused {
        Style::default().fg(theme.border_focused)
    } else {
        Style::default()
            .fg(theme.border)
            .add_modifier(Modifier::DIM)
    }
}

fn render_home_list(frame: &mut Frame, area: Rect, app: &mut App) {
    // Stash the inner height (visible rows inside the borders) so the pure
    // PgUp/PgDn handler pages by the live viewport, like the detail/preview panes.
    app.sync_list_viewport(area.height.saturating_sub(2));
    // Keep the stateful selection/offset in step with the cursor so the
    // selected row is always on screen and the table scrolls under it.
    app.sync_table_state();
    let theme = &app.theme;
    let border = pane_border(theme, app.focus == Focus::List);

    // With search results, show them. Otherwise fall back to recents, then a hint.
    if !app.view.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Results ")
            .border_style(border);
        let rows: Vec<Row> = app
            .view
            .iter()
            .filter_map(|&idx| app.results.get(idx))
            .map(|r| result_row(r, theme))
            .collect();
        let table = results_table(rows, block, theme);
        frame.render_stateful_widget(table, area, &mut app.table_state);
        return;
    }

    if !app.recent.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Recent ")
            .border_style(border);
        // Recents carry only a kind and a title; the SITE column is empty.
        let rows: Vec<Row> = app
            .recent
            .iter()
            .map(|item| {
                Row::new([
                    Cell::from(item.kind.as_str()),
                    Cell::from(item.title.clone()),
                    Cell::from(""),
                ])
            })
            .collect();
        let table = results_table(rows, block, theme);
        frame.render_stateful_widget(table, area, &mut app.table_state);
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Results ")
        .border_style(border);
    frame.render_widget(
        Paragraph::new("Press / to search NetBox.")
            .block(block)
            .style(Style::default().fg(theme.text_dim)),
        area,
    );
}

/// The right pane: a live peek at the highlighted result. Shows the full loaded
/// detail when available, otherwise a lightweight placeholder built from the row.
fn render_home_preview(frame: &mut Frame, area: Rect, app: &mut App) {
    // Stash the inner height so the pure scroll handler can clamp at the bottom.
    let inner_height = area.height.saturating_sub(2);
    app.sync_preview_viewport(inner_height);

    let theme = &app.theme;
    let border = pane_border(theme, app.focus == Focus::Preview);
    let title = app.preview_title();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(border);

    let body = app.preview_body();
    let lines = body_lines(&body, theme);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(theme.text))
            .scroll((app.preview_scroll, 0)),
        area,
    );
}

/// The widest a key/value label is allowed to grow before alignment stops
/// padding to it — so one freakishly long label can't push every value off the
/// pane. Beyond this, over-long labels keep their own width and their value
/// simply follows after a single space.
const LABEL_CAP: usize = 16;

/// Turn a plain-text detail/preview body into colored, column-aligned lines.
///
/// `key: value` rows have their labels padded to a uniform column (the width of
/// the widest label in this body, capped at [`LABEL_CAP`]) so the values line up
/// — the "Status        ● active" look. The padding is the only thing added; the
/// label and value text are untouched. Lines that aren't `key: value` (blank
/// lines, headers, tab tables) pass straight through. `status:`/`enabled:` rows
/// keep T4's value coloring (active→green, offline→red, …); the alignment never
/// changes a value's color, only the whitespace before it.
fn body_lines<'a>(body: &'a str, theme: &Theme) -> Vec<Line<'a>> {
    let width = label_width(body);
    body.lines().map(|l| kv_line(l, width, theme)).collect()
}

/// Split a body line into its `(label, value)` halves on the FIRST `": "`. Only
/// the first separator counts, so a value that itself contains `: ` (a URL, an
/// IPv6 address) stays intact. `None` for lines that aren't a `key: value` row.
fn split_kv(line: &str) -> Option<(&str, &str)> {
    line.split_once(": ")
}

/// The label column width to align this body's `key: value` rows to: the widest
/// label present, clamped to [`LABEL_CAP`]. Pure so it can be tested directly.
fn label_width(body: &str) -> usize {
    body.lines()
        .filter_map(split_kv)
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0)
        .min(LABEL_CAP)
}

/// Render one body line with its label padded to `width` so values align into a
/// column. Non-`key: value` lines pass through unchanged. `status`/`enabled`
/// values keep their status-palette color (T4); every other value is plain text.
fn kv_line<'a>(line: &'a str, width: usize, theme: &Theme) -> Line<'a> {
    let Some((label, value)) = split_kv(line) else {
        return Line::from(line.to_string());
    };
    // Pad the "label:" so the values start at a common column. Labels longer than
    // the cap keep their natural width (pad saturates to 0), trailing a value
    // after a single space rather than blowing the column open.
    let pad = width.saturating_sub(label.chars().count());
    let label_cell = format!("{label}:{:pad$} ", "", pad = pad);

    let value_style = match label {
        "status" => theme.status_style(value),
        // Map the enabled flag onto a healthy/down status so true reads green and
        // false reads red, reusing the same palette.
        "enabled" => theme.status_style(match value.trim() {
            "true" => "active",
            "false" => "offline",
            other => other,
        }),
        _ => Style::default(),
    };
    Line::from(vec![
        Span::raw(label_cell),
        Span::styled(value.to_string(), value_style),
    ])
}

/// The styled (text, style) pairs for a result row's three cells, in column
/// order: KIND / DISPLAY / SITE. The kind tag is dimmed to recede behind the
/// display label; the SITE cell is colored via [`Theme::status_style`] when its
/// value reads like a status (so a status surfaced through the subtitle keeps
/// T4's palette) and stays neutral text otherwise; the display is plain. Pure
/// (no widgets), so the cell text + color decisions are unit-testable.
fn result_row_styled(
    result: &crate::netbox::search::SearchResult,
    theme: &Theme,
) -> [(String, Style); 3] {
    let [kind, display, site] = result_row_cells(result);
    let site_style = theme.status_style(&site);
    [
        (kind, Style::default().fg(theme.text_dim)),
        (display, Style::default()),
        (site, site_style),
    ]
}

/// One aligned results row built from the pure [`result_row_styled`] cells.
fn result_row<'a>(result: &crate::netbox::search::SearchResult, theme: &Theme) -> Row<'a> {
    let cells = result_row_styled(result, theme);
    Row::new(cells.map(|(text, style)| Cell::from(text).style(style)))
}

/// The column header row (`KIND  DISPLAY  SITE`), dim + bold so it reads as a
/// label band above the aligned cells.
fn results_header<'a>(theme: &Theme) -> Row<'a> {
    Row::new([
        Cell::from("KIND"),
        Cell::from("DISPLAY"),
        Cell::from("SITE"),
    ])
    .style(
        Style::default()
            .fg(theme.text_dim)
            .add_modifier(Modifier::BOLD),
    )
}

/// A stateful table with the project's selection marker/highlight and a dim/bold
/// header row. ratatui aligns the cells into columns from the [`Constraint`]s and
/// draws the `highlight_symbol`/`row_highlight_style` on the row matching
/// `TableState`'s selection, scrolling the offset to keep it visible — exactly
/// the selection-stays-visible behaviour the old `List`/`ListState` had.
fn results_table<'a>(rows: Vec<Row<'a>>, block: Block<'a>, theme: &Theme) -> Table<'a> {
    let widths = [
        Constraint::Length(KIND_COL),
        Constraint::Min(1),
        Constraint::Length(SITE_COL),
    ];
    Table::new(rows, widths)
        .header(results_header(theme))
        .block(block)
        .style(Style::default().fg(theme.text))
        .highlight_symbol("> ")
        .row_highlight_style(Style::default().fg(theme.text).bg(theme.highlight_bg))
}

/// The keybindings shown in the `?`/`F1` help overlay, grouped into the columns
/// the cheese `Help` grid lays out side by side. Pure data, kept TRUTHFUL to
/// what the key handlers actually bind (see `state::App::handle_normal_key`) —
/// no aspirational bindings. Split out so the content is unit-testable without a
/// terminal; the render path feeds it straight to [`cheese::Help::new`].
pub fn help_bindings() -> Vec<Vec<(&'static str, &'static str)>> {
    vec![
        // Navigation / search.
        vec![
            ("/", "search"),
            (":", "command palette"),
            ("Tab / S-Tab", "switch pane"),
            ("j / k", "move / scroll"),
            ("g / G", "top / bottom"),
            ("PgUp / PgDn", "page up / down"),
            ("Enter", "open detail"),
        ],
        // Actions / detail tabs / app.
        vec![
            ("o", "open in browser"),
            ("y", "copy"),
            ("t", "cycle theme"),
            ("r", "refresh"),
            ("i p c v s", "device tabs"),
            ("b / Esc", "back"),
            ("? / F1", "toggle help"),
            ("q", "quit"),
        ],
    ]
}

fn render_help(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build the cheese Help grid from the real keybindings and center it
    // vertically within the bordered inner area.
    let columns = help_bindings();
    let column_refs: Vec<&[(&str, &str)]> = columns.iter().map(std::vec::Vec::as_slice).collect();
    let help = crate::tui::cheese::Help::new(&column_refs);

    let h = help.required_height().min(inner.height);
    let top = inner.y + inner.height.saturating_sub(h) / 2;
    let grid = Rect::new(inner.x, top, inner.width, h);
    help.render(frame, grid, theme);
}

fn render_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    // Inner height (rows for content) is the pane minus the top/bottom borders.
    // Stash it so the pure scroll handler can clamp at the bottom, and re-clamp
    // the current offset in case the pane just shrank under it.
    let inner_height = area.height.saturating_sub(2);
    app.sync_detail_viewport(inner_height);

    let theme = &app.theme;
    let title = match &app.detail {
        Some(d) => d.title.as_str(),
        None => "Detail",
    };
    // The detail screen is the active view, so it wears the focused-border color
    // — the same focused/normal convention the home panes use (see `pane_border`).
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(pane_border(theme, true));

    let mut lines: Vec<Line> = Vec::new();
    if let Some(d) = &app.detail
        && !d.tabs.is_empty()
    {
        lines.push(tab_bar(app, d));
        lines.push(Line::from(""));
    }
    lines.extend(body_lines(app.detail_body(), theme));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(theme.text))
            .scroll((app.detail_scroll, 0)),
        area,
    );
}

/// A tab bar like `[summary]  i:interfaces  p:ips`, active tab highlighted.
fn tab_bar<'a>(app: &App, detail: &'a crate::domain::detail::DetailView) -> Line<'a> {
    let theme = &app.theme;
    let style = |active: bool| {
        if active {
            Style::default().fg(theme.text).bg(theme.highlight_bg)
        } else {
            Style::default().fg(theme.text_dim)
        }
    };
    let mut spans = vec![Span::styled(" summary ", style(app.detail_tab == 0))];
    for (i, tab) in detail.tabs.iter().enumerate() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!(" {}:{} ", tab.key, tab.label.to_lowercase()),
            style(app.detail_tab == i + 1),
        ));
    }
    Line::from(spans)
}

fn render_footer(frame: &mut Frame, area: Rect, app: &mut App) {
    // In Search/Command mode the footer is the cheese-backed line editor: it
    // draws the `sigil value` line itself (with a visible cursor) and reports
    // where the terminal cursor should sit, which we then place. The borrow of
    // `app.theme` is cloned out first so the input can borrow `app` mutably.
    match app.mode {
        Mode::Search => {
            let theme = app.theme.clone();
            let pos = app.search_input.render(frame, area, '/', &theme);
            frame.set_cursor_position(pos);
            return;
        }
        Mode::Command => {
            let theme = app.theme.clone();
            let pos = app.command_input.render(frame, area, ':', &theme);
            frame.set_cursor_position(pos);
            return;
        }
        Mode::Normal => {}
    }

    let theme = &app.theme;
    let mut spans: Vec<Span> = Vec::new();
    // While a request is in flight, lead the status line with the loading
    // spinner glyph (styled via the cheese palette). When idle the footer is
    // exactly as before — no spinner cell.
    if app.loading() {
        spans.push(Span::raw(" "));
        spans.push(app.spinner.span(theme));
    }
    if !app.status.is_empty() {
        // A live status message is colored by its severity: errors red, partial
        // results yellow, confirmations green, ordinary chatter dim.
        spans.push(Span::styled(
            format!(" {} ", app.status),
            theme.message_style(app.status_severity),
        ));
    } else if app.loading() {
        // Loading with no specific status: a neutral hint beside the spinner.
        spans.push(Span::styled(
            " loading… ",
            Style::default().fg(theme.text_dim),
        ));
    } else {
        spans.push(Span::styled(
            " / search   Tab pane   Enter open   o browser   y copy   b back   t theme   ? help   q quit ",
            Style::default().fg(theme.text_dim),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().fg(theme.text)),
        area,
    );
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "normal",
        Mode::Search => "search",
        Mode::Command => "command",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull the foreground color the line's last span renders in.
    fn last_fg(line: &Line) -> Option<ratatui::style::Color> {
        line.spans.last().and_then(|s| s.style.fg)
    }

    /// The text of the line's last span (the value half of a `key: value` row).
    fn value_text(line: &Line) -> String {
        line.spans
            .last()
            .map(|s| s.content.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn kv_line_colors_the_value_by_status_palette() {
        let theme = Theme::default_theme();
        // active → success; offline → error; planned → warning. Width 0 = no
        // padding; coloring is independent of alignment.
        assert_eq!(
            last_fg(&kv_line("status: active", 0, &theme)),
            Some(theme.success)
        );
        assert_eq!(
            last_fg(&kv_line("status: offline", 0, &theme)),
            Some(theme.error)
        );
        assert_eq!(
            last_fg(&kv_line("status: planned", 0, &theme)),
            Some(theme.warning)
        );
        // Unknown status stays neutral text.
        assert_eq!(
            last_fg(&kv_line("status: whatever", 0, &theme)),
            Some(theme.text)
        );
    }

    #[test]
    fn kv_line_colors_enabled_flag() {
        let theme = Theme::default_theme();
        assert_eq!(
            last_fg(&kv_line("enabled: true", 0, &theme)),
            Some(theme.success)
        );
        assert_eq!(
            last_fg(&kv_line("enabled: false", 0, &theme)),
            Some(theme.error)
        );
    }

    #[test]
    fn kv_line_leaves_non_status_values_uncolored() {
        let theme = Theme::default_theme();
        // A non-status key/value row colors its value with the default style (no
        // explicit fg) — the value text is never altered, only the column it
        // starts at. The label cell is the leading span; the value is the last.
        let line = kv_line("name: edge01", 0, &theme);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(last_fg(&line), None);
        assert_eq!(value_text(&line), "edge01");
    }

    #[test]
    fn kv_line_passes_through_non_kv_lines() {
        let theme = Theme::default_theme();
        // A line with no `": "` separator is emitted verbatim as one span.
        let line = kv_line("--- interfaces ---", 8, &theme);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(value_text(&line), "--- interfaces ---");
    }

    #[test]
    fn kv_line_splits_on_first_separator_keeping_value_intact() {
        let theme = Theme::default_theme();
        // A value containing its own `: ` (an IPv6 with a space-delimited note)
        // is split only on the first separator, so the value survives whole.
        let line = kv_line("primary_ip6: 2001:db8:: gateway", 0, &theme);
        assert_eq!(value_text(&line), "2001:db8:: gateway");
    }

    #[test]
    fn label_width_is_max_label_capped() {
        // The column aligns to the widest label present…
        let body = "name: a\nstatus: b\nplatform: c"; // labels 4 / 6 / 8
        assert_eq!(label_width(body), 8);
        // …but never past the cap, however long a stray label gets.
        let long = "x".repeat(40);
        let body = format!("{long}: v\nname: a");
        assert_eq!(label_width(&body), LABEL_CAP);
        // A body with no key/value rows needs no column.
        assert_eq!(label_width("just a header line"), 0);
    }

    #[test]
    fn body_lines_pads_labels_into_a_uniform_column() {
        let theme = Theme::default_theme();
        // "name" (4) and "platform" (8) pad to the same label-cell width so both
        // values start at the same column. Cell = "label:" + pad + one space.
        let lines = body_lines("name: edge01\nplatform: linux", &theme);
        let name_cell = lines[0].spans[0].content.to_string();
        let plat_cell = lines[1].spans[0].content.to_string();
        assert_eq!(name_cell.chars().count(), plat_cell.chars().count());
        // The longest label sets the column: "platform:" + a trailing space.
        assert_eq!(plat_cell, "platform: ");
        assert_eq!(name_cell, "name:     ");
        // Values are intact and start at the aligned column.
        assert_eq!(value_text(&lines[0]), "edge01");
        assert_eq!(value_text(&lines[1]), "linux");
    }

    #[test]
    fn body_lines_preserves_line_count() {
        let theme = Theme::default_theme();
        let body = "name: edge01\nstatus: active\nsite: iad1";
        assert_eq!(body_lines(body, &theme).len(), 3);
    }

    #[test]
    fn help_bindings_reflect_the_real_keys() {
        // The help overlay is built from this list, so it must mirror what the
        // key handlers actually bind (state::App::handle_normal_key) — no
        // aspirational bindings. Assert each real key/description is present.
        let all: Vec<(&str, &str)> = help_bindings().into_iter().flatten().collect();
        let has = |key: &str| all.iter().any(|(k, _)| *k == key);
        // Search / palette modes.
        assert!(has("/"), "/ search");
        assert!(has(":"), ": command palette");
        // Focus + movement (incl. the recently-added Tab/Shift-Tab + paging).
        assert!(has("Tab / S-Tab"), "Tab/Shift+Tab pane focus");
        assert!(has("j / k"), "j/k move/scroll");
        assert!(has("g / G"), "g/G top/bottom");
        assert!(has("PgUp / PgDn"), "PgUp/PgDn paging");
        assert!(has("Enter"), "Enter open detail");
        // Actions.
        assert!(has("o"), "o open in browser");
        assert!(has("y"), "y copy");
        assert!(has("t"), "t cycle theme");
        assert!(has("r"), "r refresh");
        // The device-tab keys i/p/c/v/s.
        assert!(has("i p c v s"), "device tab keys");
        // Back / help / quit.
        assert!(has("b / Esc"), "b/Esc back");
        assert!(has("? / F1"), "?/F1 toggle help");
        assert!(has("q"), "q quit");
    }

    #[test]
    fn help_bindings_have_no_aspirational_keys() {
        // Guard against advertising bindings the handlers don't implement. Every
        // listed key maps to a real handler arm; spot-check that obvious unbound
        // keys are absent.
        let all: Vec<(&str, &str)> = help_bindings().into_iter().flatten().collect();
        let keys: Vec<&str> = all.iter().map(|(k, _)| *k).collect();
        for bogus in ["x", "d", "F2", "Ctrl+R", "n"] {
            assert!(
                !keys.contains(&bogus),
                "help must not advertise unbound key {bogus}"
            );
        }
    }

    #[test]
    fn result_row_styled_builds_kind_display_site_cells() {
        use crate::netbox::search::{ObjectKind, SearchResult};
        let theme = Theme::default_theme();
        let r = SearchResult {
            kind: ObjectKind::Device,
            id: 1,
            display: "edge01".into(),
            subtitle: Some("iad1".into()),
            url: "http://nb/dcim/devices/1/".into(),
            score: 100,
        };
        // Three aligned cells, in column order: KIND / DISPLAY / SITE.
        let cells = result_row_styled(&r, &theme);
        assert_eq!(cells[0].0, "device");
        assert_eq!(cells[1].0, "edge01");
        assert_eq!(cells[2].0, "iad1");
        // The kind cell recedes (dim); the display uses the table's base text.
        assert_eq!(cells[0].1.fg, Some(theme.text_dim));
        assert_eq!(cells[1].1.fg, None);
    }

    #[test]
    fn result_row_styled_colors_a_status_like_site() {
        use crate::netbox::search::{ObjectKind, SearchResult};
        let theme = Theme::default_theme();
        // When the SITE cell (from the subtitle) reads like a status, it picks up
        // T4's palette; an ordinary site name stays neutral text.
        let active = SearchResult {
            kind: ObjectKind::Device,
            id: 1,
            display: "x".into(),
            subtitle: Some("active".into()),
            url: "u".into(),
            score: 1,
        };
        assert_eq!(
            result_row_styled(&active, &theme)[2].1.fg,
            Some(theme.success)
        );
        let plain = SearchResult {
            subtitle: Some("iad1".into()),
            ..active
        };
        assert_eq!(result_row_styled(&plain, &theme)[2].1.fg, Some(theme.text));
    }
}
