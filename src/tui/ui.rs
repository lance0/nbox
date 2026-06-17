//! TUI rendering.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::tui::state::{App, Focus, Mode, Screen};
use crate::tui::theme::Theme;

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
    // selected row is always on screen and the list scrolls under it.
    app.sync_list_state();
    let theme = &app.theme;
    let border = pane_border(theme, app.focus == Focus::List);

    // With search results, show them. Otherwise fall back to recents, then a hint.
    if !app.view.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Results ")
            .border_style(border);
        let items: Vec<ListItem> = app
            .view
            .iter()
            .filter_map(|&idx| app.results.get(idx))
            .map(|r| {
                let text = match &r.subtitle {
                    Some(s) => format!("{:<7} {}  ({s})", r.kind.as_str(), r.display),
                    None => format!("{:<7} {}", r.kind.as_str(), r.display),
                };
                ListItem::new(text)
            })
            .collect();
        let list = results_list(items, block, theme);
        frame.render_stateful_widget(list, area, &mut app.list_state);
        return;
    }

    if !app.recent.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Recent ")
            .border_style(border);
        let items: Vec<ListItem> = app
            .recent
            .iter()
            .map(|item| ListItem::new(item.title.clone()))
            .collect();
        let list = results_list(items, block, theme);
        frame.render_stateful_widget(list, area, &mut app.list_state);
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

/// Turn a plain-text detail/preview body into colored lines. Lines are passed
/// through untouched except for `status:`/`enabled:` `key: value` rows, whose
/// VALUE is colored by the theme's status palette (active→green, offline→red,
/// planned→yellow, …). The text itself is never altered — only its color.
fn body_lines<'a>(body: &'a str, theme: &Theme) -> Vec<Line<'a>> {
    body.lines().map(|l| status_line(l, theme)).collect()
}

/// Color one body line. A `status: <value>` (or `enabled: true/false`) line gets
/// its value styled via [`Theme::status_style`]; everything else stays plain.
fn status_line<'a>(line: &'a str, theme: &Theme) -> Line<'a> {
    if let Some(rest) = line.strip_prefix("status: ") {
        return Line::from(vec![
            Span::raw("status: "),
            Span::styled(rest, theme.status_style(rest)),
        ]);
    }
    if let Some(rest) = line.strip_prefix("enabled: ") {
        // Map the enabled flag onto a healthy/down status so ✓/true reads green
        // and ✗/false reads red, reusing the same palette.
        let proxy = match rest.trim() {
            "true" => "active",
            "false" => "offline",
            other => other,
        };
        return Line::from(vec![
            Span::raw("enabled: "),
            Span::styled(rest, theme.status_style(proxy)),
        ]);
    }
    Line::from(line.to_string())
}

/// A stateful list with the project's selection marker/highlight. ratatui draws
/// the `highlight_symbol`/`highlight_style` on the row matching `ListState`'s
/// selection and scrolls the offset to keep it visible.
fn results_list<'a>(items: Vec<ListItem<'a>>, block: Block<'a>, theme: &Theme) -> List<'a> {
    List::new(items)
        .block(block)
        .style(Style::default().fg(theme.text))
        .highlight_symbol("> ")
        .highlight_style(Style::default().fg(theme.text).bg(theme.highlight_bg))
}

fn render_help(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from("nbox — keybindings"),
        Line::from(""),
        Line::from("/        search"),
        Line::from(":        command palette"),
        Line::from("Tab      switch list / preview pane"),
        Line::from("j / k    move selection (scroll focused pane / detail body)"),
        Line::from("g / G    top / bottom"),
        Line::from("PgUp/PgDn  page up / down"),
        Line::from("Enter    open full detail"),
        Line::from("t        cycle theme"),
        Line::from("i/p/c/v/s  device tabs (interfaces/IPs/cables/VLANs/services)"),
        Line::from("b / Esc  back"),
        Line::from("?  / F1  toggle this help"),
        Line::from("q        quit"),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(theme.border));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(theme.text)),
        area,
    );
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

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let line = match app.mode {
        Mode::Search => Line::from(format!("/{}", app.search_input)),
        Mode::Command => Line::from(format!(":{}", app.command_input)),
        // A live status message is colored by its severity: errors red, partial
        // results yellow, confirmations green, ordinary chatter dim.
        Mode::Normal if !app.status.is_empty() => Line::from(Span::styled(
            format!(" {} ", app.status),
            theme.message_style(app.status_severity),
        )),
        Mode::Normal => Line::from(Span::styled(
            " / search   Tab pane   Enter open   o browser   y copy   b back   t theme   ? help   q quit ",
            Style::default().fg(theme.text_dim),
        )),
    };
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(theme.text)),
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

    #[test]
    fn status_line_colors_the_value_by_status_palette() {
        let theme = Theme::default_theme();
        // active → success; offline → error; planned → warning.
        assert_eq!(
            last_fg(&status_line("status: active", &theme)),
            Some(theme.success)
        );
        assert_eq!(
            last_fg(&status_line("status: offline", &theme)),
            Some(theme.error)
        );
        assert_eq!(
            last_fg(&status_line("status: planned", &theme)),
            Some(theme.warning)
        );
        // Unknown status stays neutral text.
        assert_eq!(
            last_fg(&status_line("status: whatever", &theme)),
            Some(theme.text)
        );
    }

    #[test]
    fn status_line_colors_enabled_flag() {
        let theme = Theme::default_theme();
        assert_eq!(
            last_fg(&status_line("enabled: true", &theme)),
            Some(theme.success)
        );
        assert_eq!(
            last_fg(&status_line("enabled: false", &theme)),
            Some(theme.error)
        );
    }

    #[test]
    fn status_line_leaves_other_lines_plain() {
        let theme = Theme::default_theme();
        // A non-status line is a single unstyled span.
        let line = status_line("name: edge01", &theme);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(last_fg(&line), None);
    }

    #[test]
    fn body_lines_preserves_line_count() {
        let theme = Theme::default_theme();
        let body = "name: edge01\nstatus: active\nsite: iad1";
        assert_eq!(body_lines(body, &theme).len(), 3);
    }
}
