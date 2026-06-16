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
            format!("netbox: {} ", app.base_url),
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Results ")
        .border_style(Style::default().fg(theme.border));

    if app.results.is_empty() {
        let hint = Paragraph::new("Press / to search NetBox.")
            .block(block)
            .style(Style::default().fg(theme.text_dim));
        frame.render_widget(hint, area);
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let marker = if i == app.selected { "> " } else { "  " };
            let text = match &r.subtitle {
                Some(s) => format!("{marker}{:<7} {}  ({s})", r.kind.as_str(), r.display),
                None => format!("{marker}{:<7} {}", r.kind.as_str(), r.display),
            };
            let style = if i == app.selected {
                Style::default().fg(theme.text).bg(theme.highlight_bg)
            } else {
                Style::default().fg(theme.text)
            };
            ListItem::new(text).style(style)
        })
        .collect();

    frame.render_widget(List::new(items).block(block), area);
}

fn render_help(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from("nbx — keybindings"),
        Line::from(""),
        Line::from("/        search"),
        Line::from(":        command palette"),
        Line::from("j / k    move selection"),
        Line::from("g / G    top / bottom"),
        Line::from("t        cycle theme"),
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
    let (title, body) = match &app.detail {
        Some(d) => (d.title.as_str(), d.body.as_str()),
        None => ("Detail", "loading…"),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(theme.border_focused));
    frame.render_widget(
        Paragraph::new(body)
            .block(block)
            .style(Style::default().fg(theme.text)),
        area,
    );
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
