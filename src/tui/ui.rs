//! TUI rendering.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table};

use crate::tui::config_modal::{ConfigModal, ConfigSection, ProfilesMode, TestState};
use crate::tui::state::{App, Focus, Modal, Mode, Screen, result_row_cells};
use crate::tui::theme::Theme;

/// Column widths for the results table. KIND is fixed-width so the kind tags line
/// up; DISPLAY flexes to fill the middle; SITE gets a fixed tail column. These
/// give ratatui the [`Constraint`]s it needs to align the cells into columns.
const KIND_COL: u16 = 8;
const SITE_COL: u16 = 14;

/// A compact scroll-position hint for a scrollable pane's title, e.g. `" 23% "`,
/// or `None` when all the content fits (nothing scrolls, so no hint is shown).
///
/// `offset` is the first visible line, `content` the total line count, `viewport`
/// the visible rows. The percentage is the offset's progress through the
/// scrollable span (`content - viewport`): `0%` pinned to the top, `100%` once
/// the last line is at the bottom. Pure (no widgets), so it's unit-testable.
fn scroll_hint(offset: u16, content: usize, viewport: u16) -> Option<String> {
    let content = content as u16;
    let span = content.saturating_sub(viewport);
    // Everything fits (or the viewport isn't known yet): nothing to scroll.
    if span == 0 || viewport == 0 {
        return None;
    }
    let offset = offset.min(span);
    // Round to the nearest percent so the bottom reads a clean 100%.
    let pct = (u32::from(offset) * 100 + u32::from(span) / 2) / u32::from(span);
    Some(format!(" {pct}% "))
}

/// A compact row-position hint for the results list's title, e.g. `" 3/47 "`, or
/// `None` for an empty list. `selected` is 0-based; the hint is 1-based so it
/// reads naturally ("row 3 of 47"). Pure and unit-testable.
fn list_position(selected: usize, len: usize) -> Option<String> {
    if len == 0 {
        return None;
    }
    Some(format!(" {}/{len} ", selected.min(len - 1) + 1))
}

/// Render the whole UI for the current frame.
pub fn render(frame: &mut Frame, app: &mut App) {
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    render_header(frame, areas[0], app);

    match app.screen {
        Screen::Home => render_home(frame, areas[1], app),
        Screen::Detail => render_detail(frame, areas[1], app),
    }

    render_footer(frame, areas[2], app);

    // A modal floats over the whole frame, on top of the live screen — it's an
    // overlay, not its own page (mirrors ttl/xfr). Drawn last so it sits above
    // everything; `Clear` only wipes the popup rect, leaving the rest of the UI
    // visible behind/around it.
    let area = frame.area();
    let theme = app.theme.clone();
    // The profile list + active marker the Config modal renders come from the live
    // app; capture them before the mutable borrow of `app.modal` (the form render
    // needs `&mut` for its cursor placement).
    let names: Vec<String> = app.profiles.iter().map(|p| p.name.clone()).collect();
    let active = app.profile_name.clone();
    match &mut app.modal {
        Some(Modal::Help) => render_help(frame, area, &theme),
        Some(Modal::Config(modal)) => {
            render_config(frame, area, modal, &names, &active, &theme);
        }
        None => {}
    }
}

/// The top status bar: `profile:` + `netbox:` URL/version on the left, the active
/// `mode:` right-aligned, all on a subtle `chrome_bg` fill so it reads as a bar
/// rather than loose text floating on the terminal background. The profile name
/// is emphasized (the one value you switch); the URL/version stay dim.
fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let bar = Style::default().bg(theme.chrome_bg);
    // Fill the whole row first so the bar spans edge to edge behind both segments.
    frame.render_widget(Block::default().style(bar), area);

    let left = Line::from(vec![
        Span::styled(" profile: ", bar.fg(theme.text_dim)),
        Span::styled(
            app.profile_name.clone(),
            bar.fg(theme.header).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  netbox: ", bar.fg(theme.text_dim)),
        Span::styled(format!("{} ", app.base_url), bar.fg(theme.text)),
        Span::styled(format!("(v{})", app.netbox_version), bar.fg(theme.text_dim)),
    ]);
    let mode = format!(" mode: {} ", mode_label(app.mode));
    let mode_w = mode.chars().count().try_into().unwrap_or(u16::MAX);
    let cols = Layout::horizontal([Constraint::Min(0), Constraint::Length(mode_w)]).split(area);
    frame.render_widget(Paragraph::new(left).style(bar), cols[0]);
    frame.render_widget(
        Paragraph::new(Span::styled(mode, bar.fg(theme.accent)))
            .style(bar)
            .alignment(Alignment::Right),
        cols[1],
    );
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

    // A row-position hint (`selected/len`) for the title corner, so a long list
    // reads as "row 3 of 47" rather than an unbounded scroll. Computed once from
    // the active list length; absent for an empty list.
    let position = list_position(app.selected, app.home_len());

    // With search results, show them. Otherwise fall back to recents, then a hint.
    if !app.view.is_empty() {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(" Results ")
            .border_style(border)
            .padding(Padding::horizontal(1));
        if let Some(pos) = position {
            block = block.title(Line::from(pos).right_aligned().style(theme.text_dim));
        }
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
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(" Recent ")
            .border_style(border)
            .padding(Padding::horizontal(1));
        if let Some(pos) = position {
            block = block.title(Line::from(pos).right_aligned().style(theme.text_dim));
        }
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
        .border_style(border)
        .padding(Padding::horizontal(1));
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
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(border)
        .padding(Padding::horizontal(1));
    // Fetch the body once (M10: it borrows the loaded detail rather than cloning)
    // and reuse it for both the scroll hint's line count and the rendered lines.
    let body = app.preview_body();
    // Same scroll-position hint as the detail pane when the peek overflows.
    if let Some(hint) = scroll_hint(app.preview_scroll, body.lines().count(), inner_height) {
        block = block.title(Line::from(hint).right_aligned().style(theme.text_dim));
    }

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
/// the modal stacks as blank-line-separated groups. Pure data, kept TRUTHFUL to
/// what the key handlers actually bind (see `state::App::handle_normal_key`) —
/// no aspirational bindings. Split out so the content is unit-testable without a
/// terminal; the render path feeds it straight to [`help_lines`].
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
            ("P / C-P", "switch profile"),
            ("S", "config / profiles"),
            ("i p c v s", "device tabs"),
            ("b / Esc", "back"),
            ("? / F1", "toggle help"),
            ("q", "quit"),
        ],
    ]
}

/// The dim footer line that closes the help modal, matching ttl's wording.
const HELP_CLOSE_HINT: &str = "  Press any key to close";

/// Build the help modal's body lines from the grouped keybindings: each row is
/// the key padded to a uniform accent-colored column, then its description in
/// normal text; groups are separated by a blank line. A leading blank line and
/// the dim "press any key to close" footer bracket the list — the same compact,
/// centered look ttl uses. `key_col` is the padded width of the key column (the
/// widest key + a little breathing room; see [`help_key_col_width`]).
///
/// Pure (no widgets, no terminal): it returns the styled [`Line`]s the modal
/// paints, so the content/layout can be unit-tested directly.
fn help_lines<'a>(
    groups: &'a [Vec<(&'a str, &'a str)>],
    key_col: usize,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = vec![Line::from("")];
    for (gi, group) in groups.iter().enumerate() {
        if gi > 0 {
            lines.push(Line::from(""));
        }
        for (key, desc) in group {
            // "  <key>   " in the accent color, then the description in plain text
            // — ttl's `"  q       "` shortcut + `Quit` shape.
            let key_cell = format!("  {key:<key_col$}  ");
            lines.push(Line::from(vec![
                Span::styled(key_cell, Style::default().fg(theme.accent)),
                Span::styled((*desc).to_string(), Style::default().fg(theme.text)),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        HELP_CLOSE_HINT,
        Style::default().fg(theme.text_dim),
    )));
    lines
}

/// The key column's padded width: the widest key string across every group, so
/// the descriptions line up into a single column. Pure so it's testable.
fn help_key_col_width(groups: &[Vec<(&str, &str)>]) -> usize {
    groups
        .iter()
        .flatten()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0)
}

/// The full inner content width the modal needs: the widest rendered body line
/// ("  " + key column + "  " + description, and the close-hint footer). Pure.
fn help_content_width(groups: &[Vec<(&str, &str)>], key_col: usize) -> usize {
    let widest_row = groups
        .iter()
        .flatten()
        .map(|(_, desc)| 2 + key_col + 2 + desc.chars().count())
        .max()
        .unwrap_or(0);
    widest_row.max(HELP_CLOSE_HINT.chars().count())
}

/// Render the centered help modal over the full frame `area`, ttl/xfr style: a
/// content-sized popup `Rect` centered in `area`, `Clear`ed so it floats over the
/// live UI, a bordered [`Block`] titled `" Help — nbox <version> "`, and the
/// [`help_lines`] body. Sizing is clamped to the available area so it never
/// overruns a small terminal.
fn render_help(frame: &mut Frame, area: Rect, theme: &Theme) {
    let groups = help_bindings();
    let key_col = help_key_col_width(&groups);
    let lines = help_lines(&groups, key_col, theme);

    // Content-sized popup: inner content width/height + borders, clamped to the
    // frame. Width = widest line + a column of side padding; height = the line
    // count + the top/bottom border rows.
    let content_w = help_content_width(&groups, key_col) as u16;
    let popup_w = (content_w + 4).min(area.width);
    let popup_h = (lines.len() as u16 + 2).min(area.height);
    let popup_x = area.x + area.width.saturating_sub(popup_w) / 2;
    let popup_y = area.y + area.height.saturating_sub(popup_h) / 2;
    let popup = Rect::new(popup_x, popup_y, popup_w, popup_h);

    // Wipe just the popup rect so the modal floats over the dimmed live screen.
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Help — nbox {} ", crate::VERSION))
        .border_style(Style::default().fg(theme.border_focused));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(theme.text)),
        popup,
    );
}

/// A centered popup `Rect` of the given inner content size (plus borders),
/// clamped to `area`. Shared sizing for the floating modals.
fn centered_popup(area: Rect, content_w: u16, content_h: u16) -> Rect {
    let popup_w = (content_w + 4).min(area.width);
    let popup_h = (content_h + 2).min(area.height);
    let popup_x = area.x + area.width.saturating_sub(popup_w) / 2;
    let popup_y = area.y + area.height.saturating_sub(popup_h) / 2;
    Rect::new(popup_x, popup_y, popup_w, popup_h)
}

/// Render the centered Config modal over the full frame `area`. Two sections
/// (Profiles | Settings, `Tab` to switch); only Profiles is interactive — it
/// lists the configured profiles (active marked) and, in a form, edits one. The
/// modal is rendered last so it floats over the live screen (`Clear`ed rect).
fn render_config(
    frame: &mut Frame,
    area: Rect,
    modal: &mut ConfigModal,
    names: &[String],
    active: &str,
    theme: &Theme,
) {
    // L9: guard a too-small terminal. The modal needs room for its border + the
    // form rows; below that, the Layout splits would collapse and render garbage.
    // Show a compact "resize" hint instead, clamped to whatever space there is.
    const MIN_W: u16 = 24;
    const MIN_H: u16 = 8;
    if area.width < MIN_W || area.height < MIN_H {
        let popup = centered_popup(area, area.width.saturating_sub(2).min(20), 1);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "terminal too small",
                Style::default().fg(theme.text_dim),
            ))
            .block(Block::default().borders(Borders::ALL)),
            popup,
        );
        return;
    }

    let popup = centered_popup(area, 60, area.height.saturating_sub(4));
    frame.render_widget(Clear, popup);

    let section_label = match modal.section {
        ConfigSection::Profiles => "Profiles",
        ConfigSection::Settings => "Settings",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Config — {section_label} "))
        .title(
            Line::from(" Tab: section  Esc: close ")
                .right_aligned()
                .style(theme.text_dim),
        )
        .border_style(Style::default().fg(theme.border_focused));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    match modal.section {
        ConfigSection::Profiles => {
            render_config_profiles(frame, inner, modal, names, active, theme);
        }
        ConfigSection::Settings => {
            render_config_settings(frame, inner, modal, theme);
        }
    }
}

/// The Settings section body: the three real `[ui]` settings as a small form —
/// theme (a cycle), refresh_secs (numeric), open_browser_command (text). The
/// focused row is marked with `>`; the theme value shows the selection and the
/// two text rows render their (live) inputs. The no-op `confirm_writes` knob is
/// intentionally absent.
fn render_config_settings(frame: &mut Frame, area: Rect, modal: &mut ConfigModal, theme: &Theme) {
    use crate::tui::config_modal::setting;

    let s = &mut modal.settings;
    let rows = Layout::vertical([
        Constraint::Length(1), // theme
        Constraint::Length(1), // refresh_secs
        Constraint::Length(1), // open_browser_command
        Constraint::Length(1), // blank
        Constraint::Length(1), // message
        Constraint::Min(1),    // help
    ])
    .split(area);

    // A focusable label cell: `> label` when focused, `  label` otherwise.
    let label = |row: usize, text: &str| {
        let cursor = if s.focus == row { "> " } else { "  " };
        Span::styled(
            format!("{cursor}{text:<14}"),
            Style::default().fg(if s.focus == row {
                theme.header
            } else {
                theme.text_dim
            }),
        )
    };

    // theme — a cycle; show the selection and the hint.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            label(setting::THEME, "theme"),
            Span::styled(s.theme_name(), Style::default().fg(theme.accent)),
            Span::styled(
                "  (←/→ or Space cycles)",
                Style::default().fg(theme.text_dim),
            ),
        ])),
        rows[0],
    );

    // refresh_secs — a numeric text field.
    frame.render_widget(
        Paragraph::new(label(setting::REFRESH, "refresh_secs")),
        Rect::new(rows[1].x, rows[1].y, 16.min(rows[1].width), 1),
    );
    let refresh_area = Rect::new(
        rows[1].x.saturating_add(16),
        rows[1].y,
        rows[1].width.saturating_sub(16),
        1,
    );
    let refresh_cursor =
        s.refresh
            .render_with_focus(frame, refresh_area, ' ', theme, s.focus == setting::REFRESH);

    // open_browser_command — a free-text field.
    frame.render_widget(
        Paragraph::new(label(setting::BROWSER, "open command")),
        Rect::new(rows[2].x, rows[2].y, 16.min(rows[2].width), 1),
    );
    let browser_area = Rect::new(
        rows[2].x.saturating_add(16),
        rows[2].y,
        rows[2].width.saturating_sub(16),
        1,
    );
    let browser_cursor =
        s.browser
            .render_with_focus(frame, browser_area, ' ', theme, s.focus == setting::BROWSER);

    // Place the real terminal cursor on the focused text row (the theme row has
    // no text cursor).
    match s.focus {
        setting::REFRESH => frame.set_cursor_position(refresh_cursor),
        setting::BROWSER => frame.set_cursor_position(browser_cursor),
        _ => {}
    }

    if let Some(msg) = &s.message {
        frame.render_widget(
            Paragraph::new(Span::styled(msg.clone(), Style::default().fg(theme.error))),
            rows[4],
        );
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            "↑/↓: field  Enter/Ctrl+S: save  Tab: section  Esc: close",
            Style::default().fg(theme.text_dim),
        )),
        rows[5],
    );
}

/// The Profiles section body: the list, or the add/edit form, or a delete prompt.
fn render_config_profiles(
    frame: &mut Frame,
    area: Rect,
    modal: &mut ConfigModal,
    names: &[String],
    active: &str,
    theme: &Theme,
) {
    // Snapshot the list message before the mutable form borrow below.
    let list_message = modal.profiles.message.clone();
    match &mut modal.profiles.mode {
        ProfilesMode::List { selected } => {
            let mut lines: Vec<Line> = Vec::new();
            if names.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  no profiles configured — press a to add one",
                    Style::default().fg(theme.text_dim),
                )));
            }
            for (i, name) in names.iter().enumerate() {
                let is_active = name == active;
                let cursor = if i == *selected { "> " } else { "  " };
                let marker = if is_active { "* " } else { "  " };
                let style = if i == *selected {
                    Style::default().fg(theme.text).bg(theme.highlight_bg)
                } else {
                    Style::default().fg(theme.text)
                };
                lines.push(Line::from(Span::styled(
                    format!("{cursor}{marker}{name}"),
                    style,
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  a add   e edit   Enter/s select   d delete",
                Style::default().fg(theme.text_dim),
            )));
            if let Some(msg) = &list_message {
                lines.push(Line::from(Span::styled(
                    format!("  {msg}"),
                    Style::default().fg(theme.error),
                )));
            }
            frame.render_widget(Paragraph::new(lines), area);
        }
        ProfilesMode::Form(form) => {
            // Layout: the 4 form rows up top, then the auth/tls controls, the test
            // state, the help line, and an optional message.
            let rows = Layout::vertical([
                Constraint::Length(4), // the FormInput rows
                Constraint::Length(1), // auth_scheme
                Constraint::Length(1), // verify_tls
                Constraint::Length(1), // blank
                Constraint::Length(1), // test state
                Constraint::Length(1), // message
                Constraint::Min(1),    // help
            ])
            .split(area);

            if let Some(pos) = form.inputs.render(frame, rows[0], theme) {
                frame.set_cursor_position(pos);
            }

            let scheme = match form.auth_scheme {
                crate::netbox::auth::AuthScheme::Auto => "auto",
                crate::netbox::auth::AuthScheme::Bearer => "bearer",
                crate::netbox::auth::AuthScheme::Token => "token",
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("auth_scheme  ", Style::default().fg(theme.header)),
                    Span::styled(scheme, Style::default().fg(theme.accent)),
                    Span::styled("  (Ctrl+S cycles)", Style::default().fg(theme.text_dim)),
                ])),
                rows[1],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("verify_tls   ", Style::default().fg(theme.header)),
                    Span::styled(
                        if form.verify_tls { "on" } else { "off" },
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled("  (Ctrl+L toggles)", Style::default().fg(theme.text_dim)),
                ])),
                rows[2],
            );

            let (test_text, test_style) = match &form.test {
                TestState::Idle => (String::new(), Style::default().fg(theme.text_dim)),
                TestState::Testing => (
                    "testing connection…".to_string(),
                    Style::default().fg(theme.text_dim),
                ),
                TestState::Ok(v) => (
                    format!("✓ connected (NetBox v{v})"),
                    Style::default().fg(theme.success),
                ),
                TestState::Failed(e) => (format!("✗ {e}"), Style::default().fg(theme.error)),
            };
            frame.render_widget(Paragraph::new(Span::styled(test_text, test_style)), rows[4]);

            if let Some(msg) = &form.message {
                frame.render_widget(
                    Paragraph::new(Span::styled(msg.clone(), Style::default().fg(theme.error))),
                    rows[5],
                );
            }

            // Save+use is Ctrl+G (Ctrl+U is the field clear-line). On an edit form
            // also advertise Ctrl+X, which clears the stored keyring token on save.
            let help = if form.editing.is_some() {
                "Tab: field  Ctrl+T: test  Enter: save  Ctrl+G: save+use  Ctrl+X: clear token  Esc: back"
            } else {
                "Tab: field  Ctrl+T: test  Enter: save  Ctrl+G: save+use  Esc: back"
            };
            frame.render_widget(
                Paragraph::new(Span::styled(help, Style::default().fg(theme.text_dim))),
                rows[6],
            );
        }
        ProfilesMode::ConfirmDelete { name, .. } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Delete profile '{name}'?"),
                    Style::default().fg(theme.text),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  y/Enter: delete   any other key: cancel",
                    Style::default().fg(theme.text_dim),
                )),
            ];
            frame.render_widget(Paragraph::new(lines), area);
        }
    }
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
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(pane_border(theme, true))
        .padding(Padding::horizontal(1));
    // A scroll-position hint in the title corner when the body overflows, so a
    // long detail reads as scrollable rather than silently clipped.
    if let Some(hint) = scroll_hint(app.detail_scroll, app.detail_content_lines(), inner_height) {
        block = block.title(Line::from(hint).right_aligned().style(theme.text_dim));
    }

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
    // Fill the footer with the chrome bar bg first, so every mode — the nav hints,
    // the search/command editor, and the one-column padding around it — sits on the
    // same bar instead of the raw terminal background.
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.chrome_bg)),
        area,
    );

    // In Search/Command mode the footer is the cheese-backed line editor: it
    // draws the `sigil value` line itself (with a visible cursor) and reports
    // where the terminal cursor should sit, which we then place. The borrow of
    // `app.theme` is cloned out first so the input can borrow `app` mutably.
    //
    // The editor is inset one column on each side (`footer_input_area`) so the
    // `/`/`:` sigil lands at the same column as the header and the normal-mode
    // `/ search` hint — the footer reads as morphing in place rather than the
    // sigil snapping to the terminal edge.
    match app.mode {
        Mode::Search => {
            let theme = app.theme.clone();
            let pos = app
                .search_input
                .render(frame, footer_input_area(area), '/', &theme);
            frame.set_cursor_position(pos);
            return;
        }
        Mode::Command => {
            let theme = app.theme.clone();
            let pos = app
                .command_input
                .render(frame, footer_input_area(area), ':', &theme);
            frame.set_cursor_position(pos);
            return;
        }
        Mode::Normal => {}
    }

    let line = footer_line(app);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(app.theme.text).bg(app.theme.chrome_bg)),
        area,
    );
}

/// Inset a footer rect by one column on each side so the Search/Command line
/// editor's sigil aligns with the header and the normal-mode nav hint (column 1)
/// rather than hugging the terminal edge. Width floors at 0 on a tiny terminal.
fn footer_input_area(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    }
}

/// Context-sensitive normal-mode footer. Live state (spinner, result count,
/// errors, transient theme notices) gets the left edge; persistent navigation
/// follows so controls stay visible without burying the thing that just changed.
fn footer_line(app: &App) -> Line<'static> {
    let theme = &app.theme;
    let mut spans: Vec<Span> = Vec::new();
    let mut has_state = false;

    if app.loading() {
        spans.push(app.spinner.span(theme));
        spans.push(Span::raw(" "));
        has_state = true;
    }
    if !app.status.is_empty() {
        spans.push(Span::styled(
            app.status.clone(),
            theme.message_style(app.status_severity),
        ));
    } else if app.loading() {
        spans.push(Span::styled(
            "loading…",
            Style::default().fg(theme.text_dim),
        ));
    } else {
        has_state = false;
    }

    if has_state || !app.status.is_empty() {
        spans.push(Span::raw("    "));
    }
    spans.extend(nav_spans(footer_nav(app), theme));
    Line::from(spans)
}

/// Split a footer nav string (segments joined by ` · `) into styled spans: each
/// segment's leading key token in the accent color (bold), the rest dim, with dim
/// `·` separators — all on the chrome bar bg so the footer reads as one bar. Pure
/// and unit-testable.
fn nav_spans(nav: &str, theme: &Theme) -> Vec<Span<'static>> {
    let bar = Style::default().bg(theme.chrome_bg);
    let key = bar.fg(theme.accent).add_modifier(Modifier::BOLD);
    let label = bar.fg(theme.text_dim);
    let mut spans = Vec::new();
    for (i, seg) in nav
        .split('·')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        if i > 0 {
            spans.push(Span::styled(" · ", label));
        }
        match seg.split_once(char::is_whitespace) {
            Some((k, rest)) => {
                spans.push(Span::styled(k.to_string(), key));
                spans.push(Span::styled(format!(" {rest}"), label));
            }
            None => spans.push(Span::styled(seg.to_string(), key)),
        }
    }
    spans
}

fn footer_nav(app: &App) -> &'static str {
    match app.screen {
        Screen::Home if app.focus == Focus::Preview => {
            " / search · j/k scroll · g/G top/bottom · Tab results · Enter open · o/y open/copy · r refresh · ? help · q quit "
        }
        Screen::Home => {
            " / search · j/k move · Enter open · Tab preview · o/y open/copy · r refresh · t theme · ? help · q quit "
        }
        Screen::Detail => {
            " j/k scroll · g/G top/bottom · i/p/c/v/s tabs · o/y open/copy · b back · r refresh · t theme · ? help · q quit "
        }
    }
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
    use crate::config::ProfileConfig;
    use crate::netbox::client::NetBoxClient;

    fn app() -> App {
        let profile = ProfileConfig {
            url: "http://localhost".into(),
            ..Default::default()
        };
        let client = NetBoxClient::new(&profile, None).unwrap();
        App::new(
            client,
            "default",
            "test".into(),
            "http://localhost".into(),
            "4.5.5".into(),
            None,
        )
    }

    fn line_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

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
    fn footer_input_area_insets_one_column_each_side() {
        // The search/command editor sits one column in from each edge, so the
        // sigil aligns with the header instead of hugging the terminal edge.
        let full = Rect::new(0, 23, 80, 1);
        let inset = footer_input_area(full);
        assert_eq!(inset.x, 1, "left-padded by one column");
        assert_eq!(inset.width, 78, "one column trimmed off each side");
        assert_eq!((inset.y, inset.height), (23, 1), "row is unchanged");
    }

    #[test]
    fn footer_input_area_floors_width_on_a_tiny_terminal() {
        // A 1-column footer can't be inset twice; width saturates at 0, no panic.
        let inset = footer_input_area(Rect::new(0, 0, 1, 1));
        assert_eq!(inset.width, 0);
    }

    #[test]
    fn nav_spans_accents_keys_and_dims_labels() {
        let theme = Theme::default_theme();
        let spans = nav_spans(" / search · j/k move ", &theme);
        // The leading key of the first segment is accented (bold), its label dim.
        assert_eq!(spans[0].content, "/");
        assert_eq!(spans[0].style.fg, Some(theme.accent));
        assert_eq!(spans[1].content, " search");
        assert_eq!(spans[1].style.fg, Some(theme.text_dim));
        // Segments are separated, and the next segment's key is carried through.
        assert!(spans.iter().any(|s| s.content == " · "));
        assert!(
            spans
                .iter()
                .any(|s| s.content == "j/k" && s.style.fg == Some(theme.accent))
        );
        // Everything sits on the chrome bar bg.
        assert!(spans.iter().all(|s| s.style.bg == Some(theme.chrome_bg)));
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
        assert!(has("P / C-P"), "P/Ctrl+P switch profile");
        assert!(has("S"), "S config / profiles");
        // The device-tab keys i/p/c/v/s.
        assert!(has("i p c v s"), "device tab keys");
        // Back / help / quit.
        assert!(has("b / Esc"), "b/Esc back");
        assert!(has("? / F1"), "?/F1 toggle help");
        assert!(has("q"), "q quit");
    }

    #[test]
    fn help_key_col_width_is_the_widest_key() {
        // The key column pads to the widest key across every group so the
        // descriptions line up. With these two groups "Tab / S-Tab" (11) wins.
        let groups = vec![
            vec![("/", "search"), ("Tab / S-Tab", "switch pane")],
            vec![("q", "quit")],
        ];
        assert_eq!(help_key_col_width(&groups), 11);
        // No bindings → no column.
        assert_eq!(help_key_col_width(&[]), 0);
    }

    #[test]
    fn help_lines_groups_keys_and_descriptions_with_a_close_footer() {
        let theme = Theme::default_theme();
        let groups = vec![
            vec![("/", "search"), (":", "command palette")],
            vec![("q", "quit")],
        ];
        let key_col = help_key_col_width(&groups);
        let lines = help_lines(&groups, key_col, &theme);

        // Bracketed by a leading blank line and a dim "press any key to close".
        let first = lines.first().unwrap();
        assert!(
            first.spans.iter().all(|s| s.content.is_empty()),
            "leading line is blank"
        );
        let footer = lines.last().unwrap();
        assert_eq!(footer.spans[0].content, HELP_CLOSE_HINT);
        assert_eq!(footer.spans[0].style.fg, Some(theme.text_dim));

        // A binding row is a key cell (accent) + description (normal text). Find
        // the "/ search" row and check both halves and their colors.
        let row = lines
            .iter()
            .find(|l| l.spans.first().is_some_and(|s| s.content.contains('/')))
            .expect("the / row is present");
        assert_eq!(row.spans.len(), 2);
        assert!(row.spans[0].content.contains('/'));
        assert_eq!(row.spans[0].style.fg, Some(theme.accent));
        assert_eq!(row.spans[1].content, "search");
        assert_eq!(row.spans[1].style.fg, Some(theme.text));

        // The two groups are separated by a blank line: there is at least one
        // empty interior line between the first and last binding rows.
        let interior_blank = lines[1..lines.len() - 2]
            .iter()
            .any(|l| l.spans.iter().all(|s| s.content.is_empty()));
        assert!(interior_blank, "groups are separated by a blank line");

        // Every real binding from the data is represented as a row.
        let total_bindings: usize = groups.iter().map(Vec::len).sum();
        let binding_rows = lines.iter().filter(|l| l.spans.len() == 2).count();
        assert_eq!(binding_rows, total_bindings);
    }

    #[test]
    fn help_content_width_covers_widest_row_and_the_close_hint() {
        let theme = Theme::default_theme();
        // A group whose only row is narrower than the close hint: the width is
        // floored by the hint so the footer never gets clipped.
        let narrow = vec![vec![("q", "x")]];
        let key_col = help_key_col_width(&narrow);
        assert_eq!(
            help_content_width(&narrow, key_col),
            HELP_CLOSE_HINT.chars().count()
        );
        // A row wider than the hint drives the width instead.
        let wide = vec![vec![("Enter", "open a really quite long description here")]];
        let kc = help_key_col_width(&wide);
        let expected = 2 + kc + 2 + "open a really quite long description here".chars().count();
        assert_eq!(help_content_width(&wide, kc), expected);
        // Sanity: the real bindings produce a sensible (non-zero) modal width.
        let real = help_bindings();
        let rc = help_key_col_width(&real);
        assert!(help_content_width(&real, rc) > 0);
        let _ = theme; // keep the helper signature symmetric with the others
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
    fn footer_nav_is_contextual() {
        let mut a = app();
        let home = footer_nav(&a);
        assert!(home.contains("j/k move"));
        assert!(home.contains("Enter open"));

        a.focus = Focus::Preview;
        let preview = footer_nav(&a);
        assert!(preview.contains("j/k scroll"));
        assert!(preview.contains("Tab results"));

        a.screen = Screen::Detail;
        let detail = footer_nav(&a);
        assert!(detail.contains("b back"));
        assert!(detail.contains("i/p/c/v/s tabs"));
        assert!(!detail.contains("Enter open"));
    }

    #[test]
    fn footer_status_does_not_replace_navigation() {
        let mut a = app();
        a.status = "theme: nord".into();

        let text = line_text(&footer_line(&a));

        assert!(
            text.starts_with("theme: nord"),
            "status owns the left edge: {text}"
        );
        assert!(text.contains("/ search"), "nav remains present: {text}");
        let status_idx = text.find("theme: nord").expect("status present");
        let nav_idx = text.find("/ search").expect("nav present");
        assert!(status_idx < nav_idx, "status precedes navigation: {text}");
    }

    #[test]
    fn footer_loading_state_precedes_navigation() {
        let mut a = app();
        a.pending = 1;
        a.status = "searching edge…".into();

        let text = line_text(&footer_line(&a));

        assert!(text.contains("searching edge"), "status present: {text}");
        assert!(text.contains("/ search"), "nav remains present: {text}");
        let status_idx = text.find("searching edge").expect("status present");
        let nav_idx = text.find("/ search").expect("nav present");
        assert!(
            status_idx < nav_idx,
            "loading state precedes navigation: {text}"
        );
    }

    #[test]
    fn scroll_hint_is_absent_when_content_fits() {
        // Content shorter than (or equal to) the viewport doesn't scroll, so no
        // hint is shown — the pane title stays bare.
        assert_eq!(scroll_hint(0, 5, 20), None);
        assert_eq!(scroll_hint(0, 20, 20), None);
        // An unknown viewport (pre-first-render) shows nothing either.
        assert_eq!(scroll_hint(0, 100, 0), None);
    }

    #[test]
    fn scroll_hint_reports_progress_through_the_scroll_span() {
        // 30 lines in a 10-row pane → a scroll span of 20. Top = 0%, the middle
        // of the span rounds to ~50%, and the bottom is a clean 100%.
        assert_eq!(scroll_hint(0, 30, 10).as_deref(), Some(" 0% "));
        assert_eq!(scroll_hint(10, 30, 10).as_deref(), Some(" 50% "));
        assert_eq!(scroll_hint(20, 30, 10).as_deref(), Some(" 100% "));
        // An offset past the span (a stale value) still reads a clean 100%.
        assert_eq!(scroll_hint(99, 30, 10).as_deref(), Some(" 100% "));
    }

    #[test]
    fn list_position_is_one_based_and_absent_when_empty() {
        // An empty list shows no row counter.
        assert_eq!(list_position(0, 0), None);
        // Otherwise it's a 1-based "row/len" pair.
        assert_eq!(list_position(0, 47).as_deref(), Some(" 1/47 "));
        assert_eq!(list_position(2, 47).as_deref(), Some(" 3/47 "));
        assert_eq!(list_position(46, 47).as_deref(), Some(" 47/47 "));
        // A stale selection past the end is clamped to the last row.
        assert_eq!(list_position(99, 47).as_deref(), Some(" 47/47 "));
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
