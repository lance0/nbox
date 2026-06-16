//! TUI rendering.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::tui::state::{App, Mode, Screen};

/// Render the whole UI for the current frame.
pub fn render(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    let header = Line::from(vec![
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
    ]);
    frame.render_widget(Paragraph::new(header), areas[0]);

    match app.screen {
        Screen::Home => render_home(frame, areas[1], app),
        Screen::Help => render_help(frame, areas[1], app),
        Screen::Detail => render_detail(frame, areas[1], app),
    }

    render_footer(frame, areas[2], app);
}

fn render_home(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;

    // With search results, show them. Otherwise fall back to recents, then a hint.
    if !app.view.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Results ")
            .border_style(Style::default().fg(theme.border));
        let items: Vec<ListItem> = app
            .view
            .iter()
            .enumerate()
            .filter_map(|(pos, &idx)| app.results.get(idx).map(|r| (pos, r)))
            .map(|(pos, r)| {
                let text = match &r.subtitle {
                    Some(s) => format!(
                        "{}{:<7} {}  ({s})",
                        marker(pos, app),
                        r.kind.as_str(),
                        r.display
                    ),
                    None => format!("{}{:<7} {}", marker(pos, app), r.kind.as_str(), r.display),
                };
                ListItem::new(text).style(row_style(pos, app))
            })
            .collect();
        frame.render_widget(List::new(items).block(block), area);
        return;
    }

    if !app.recent.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Recent ")
            .border_style(Style::default().fg(theme.border));
        let items: Vec<ListItem> = app
            .recent
            .iter()
            .enumerate()
            .map(|(pos, item)| {
                ListItem::new(format!("{}{}", marker(pos, app), item.title))
                    .style(row_style(pos, app))
            })
            .collect();
        frame.render_widget(List::new(items).block(block), area);
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Results ")
        .border_style(Style::default().fg(theme.border));
    frame.render_widget(
        Paragraph::new("Press / to search NetBox.")
            .block(block)
            .style(Style::default().fg(theme.text_dim)),
        area,
    );
}

fn marker(pos: usize, app: &App) -> &'static str {
    if pos == app.selected { "> " } else { "  " }
}

fn row_style(pos: usize, app: &App) -> Style {
    if pos == app.selected {
        Style::default()
            .fg(app.theme.text)
            .bg(app.theme.highlight_bg)
    } else {
        Style::default().fg(app.theme.text)
    }
}

fn render_help(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from("nbox — keybindings"),
        Line::from(""),
        Line::from("/        search"),
        Line::from(":        command palette"),
        Line::from("j / k    move selection"),
        Line::from("g / G    top / bottom"),
        Line::from("t        cycle theme"),
        Line::from("i/p/c/v  device tabs (interfaces/IPs/cables/VLANs)"),
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

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let title = match &app.detail {
        Some(d) => d.title.as_str(),
        None => "Detail",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(theme.border_focused));

    let mut lines: Vec<Line> = Vec::new();
    if let Some(d) = &app.detail
        && !d.tabs.is_empty()
    {
        lines.push(tab_bar(app, d));
        lines.push(Line::from(""));
    }
    for line in app.detail_body().lines() {
        lines.push(Line::from(line.to_string()));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(theme.text)),
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
        Mode::Normal if !app.status.is_empty() => Line::from(format!(" {} ", app.status)),
        Mode::Normal => Line::from(Span::styled(
            " / search   Enter open   o browser   y copy   b back   t theme   ? help   q quit ",
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
