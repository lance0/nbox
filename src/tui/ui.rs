//! TUI rendering.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Padding, Paragraph, Row, Table};

use std::collections::HashSet;

use unicode_width::UnicodeWidthStr;

use crate::cache::Source;
use crate::netbox::prefix_tree::{self, PrefixTreeData};
use crate::netbox::search::SearchFilters;
use crate::tui::config_modal::{ConfigModal, ConfigSection, ProfilesMode, TestState};
use crate::tui::state::{App, Focus, Modal, Mode, RelatedModal, Screen, result_row_cells};
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
    // Rows are stacked top-down, each optional row taking space only when it
    // applies (zero-cost otherwise, like the scroll hints): an update banner at
    // the very top when a newer release was found, the header, a filter chips bar
    // when any filter is active, the body, and the footer.
    let show_update = app.update_available.is_some();
    let filters_on = any_filter_active(&app.filters);

    let mut constraints = Vec::with_capacity(5);
    if show_update {
        constraints.push(Constraint::Length(1)); // update banner
    }
    constraints.push(Constraint::Length(1)); // header
    if filters_on {
        constraints.push(Constraint::Length(1)); // filter chips
    }
    constraints.push(Constraint::Min(1)); // body
    constraints.push(Constraint::Length(1)); // footer
    let areas = Layout::vertical(constraints).split(frame.area());

    let mut row = 0;
    if show_update {
        render_update_banner(frame, areas[row], app);
        row += 1;
    }
    render_header(frame, areas[row], app);
    row += 1;
    if filters_on {
        render_filter_bar(frame, areas[row], &app.filters, &app.theme);
        row += 1;
    }
    let body_area = areas[row];
    let footer_area = areas[row + 1];

    match app.screen {
        Screen::Home => render_home(frame, body_area, app),
        Screen::Detail => render_detail(frame, body_area, app),
        Screen::Dashboard => render_dashboard(frame, body_area, app),
        Screen::PrefixTree => render_prefix_tree(frame, body_area, app),
    }

    render_footer(frame, footer_area, app);

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
        Some(Modal::Filter(modal)) => render_filter(frame, area, modal, &theme),
        Some(Modal::Related(modal)) => render_related(frame, area, modal, &theme),
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
        Span::styled("  profile: ", bar.fg(theme.text_dim)),
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

/// A full-width update banner at the very top when the background check found a
/// newer release. Mirrors ttl/xfr: the install-appropriate upgrade command plus a
/// `u` dismiss hint, on the theme's warning color. Only rendered when
/// `update_available` is set; `u` clears it (see `handle_normal_key`).
fn render_update_banner(frame: &mut Frame, area: Rect, app: &App) {
    let Some(version) = &app.update_available else {
        return;
    };
    let theme = &app.theme;
    // Strip a leading `v` so we never render `→ vv0.2.0` (xfr's fix).
    let new = version.strip_prefix('v').unwrap_or(version);
    let current = env!("CARGO_PKG_VERSION");
    let bar = Style::default().bg(theme.warning).fg(Color::Black);
    // Fill the row so the warning color spans edge to edge behind the text.
    frame.render_widget(Block::default().style(bar), area);
    let mut text = format!(" Update available: v{current} → v{new}");
    if !app.update_command.is_empty() {
        text.push_str("   ");
        text.push_str(app.update_command);
    }
    text.push_str("   press u to dismiss ");
    frame.render_widget(
        Paragraph::new(Span::styled(text, bar.add_modifier(Modifier::BOLD)))
            .style(bar)
            .alignment(Alignment::Center),
        area,
    );
}

/// True when any search filter is set — drives whether the chips bar row exists.
fn any_filter_active(f: &SearchFilters) -> bool {
    f.status.is_some()
        || f.site.is_some()
        || f.region.is_some()
        || f.site_group.is_some()
        || f.location.is_some()
        || f.tenant.is_some()
        || f.role.is_some()
        || f.tag.is_some()
        || f.vrf.is_some()
}

/// The filter chips bar: each active filter as a bold `[key=value]` chip on the
/// chrome bar — scope filters (mutually exclusive) in the header color, the rest in
/// the accent — with a dim palette hint. Only rendered when a filter is active.
fn render_filter_bar(frame: &mut Frame, area: Rect, f: &SearchFilters, theme: &Theme) {
    let bar = Style::default().bg(theme.chrome_bg);
    frame.render_widget(Block::default().style(bar), area);
    let mut spans = vec![Span::styled("  filters: ", bar.fg(theme.text_dim))];
    for (k, v, scope) in [
        ("status", &f.status, false),
        ("site", &f.site, true),
        ("region", &f.region, true),
        ("site-group", &f.site_group, true),
        ("location", &f.location, true),
        ("tenant", &f.tenant, false),
        ("role", &f.role, false),
        ("tag", &f.tag, false),
        ("vrf", &f.vrf, false),
    ] {
        if let Some(v) = v {
            let color = if scope { theme.header } else { theme.accent };
            spans.push(Span::styled(
                format!("[{k}={v}] "),
                bar.fg(color).add_modifier(Modifier::BOLD),
            ));
        }
    }
    spans.push(Span::styled("  :clear-filters", bar.fg(theme.text_dim)));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bar), area);
}

/// A bordered dashboard card with a dim border + one column of inner padding.
fn dash_block(title: &'static str, theme: &Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(1))
}

/// A `███░░ 92%` utilization bar, colored by severity (≥90 error, ≥75 warning,
/// else the graph color). Pure-ish — returns styled spans for one cell.
fn util_bar(pct: u8, width: u16, theme: &Theme) -> Vec<Span<'static>> {
    let width = width.max(1);
    // Floor the fill (only a true 100% fills every cell), but never let a non-zero
    // utilization round down to an empty bar — a sliver still shows one block, so
    // e.g. 10% on a narrow bar reads as a little progress rather than nothing.
    let filled = if pct == 0 {
        0
    } else {
        u16::try_from((u32::from(pct) * u32::from(width) / 100).max(1))
            .unwrap_or(width)
            .min(width)
    };
    let empty = width.saturating_sub(filled);
    let color = if pct >= 90 {
        theme.error
    } else if pct >= 75 {
        theme.warning
    } else {
        theme.graph_primary
    };
    vec![
        Span::styled("█".repeat(filled as usize), Style::default().fg(color)),
        Span::styled(
            "░".repeat(empty as usize),
            Style::default().fg(theme.text_dim),
        ),
        Span::styled(format!(" {pct:>3}%"), Style::default().fg(theme.text_dim)),
    ]
}

/// Trim an ISO-8601 timestamp to a compact `YYYY-MM-DD HH:MM` for the activity card.
fn short_time(s: &str) -> String {
    s.split('.')
        .next()
        .unwrap_or(s)
        .replace('T', " ")
        .chars()
        .take(16)
        .collect()
}

/// The overview dashboard (`D`): device status counts, top-utilized prefixes, and
/// recent journal activity. Read-only cards; falls back to a loading/error line.
fn render_dashboard(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let Some(data) = app.dashboard.as_ref() else {
        let msg = app.dashboard_error.as_deref().map_or_else(
            || "Loading dashboard…".to_string(),
            |e| format!("dashboard error: {e}"),
        );
        frame.render_widget(
            Paragraph::new(msg)
                .block(dash_block(" Dashboard ", theme))
                .style(Style::default().fg(theme.text_dim)),
            area,
        );
        return;
    };

    let rows = Layout::vertical([Constraint::Length(9), Constraint::Min(3)]).split(area);
    let top =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[0]);

    // Devices by status.
    let mut status_lines = vec![Line::from(vec![
        Span::styled("total  ", Style::default().fg(theme.text_dim)),
        Span::styled(
            data.device_total.to_string(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ])];
    for (status, count) in &data.device_status_counts {
        status_lines.push(Line::from(vec![
            Span::styled(format!("{status:<16}"), theme.status_style(status)),
            Span::styled(count.to_string(), Style::default().fg(theme.text)),
        ]));
    }
    frame.render_widget(
        Paragraph::new(status_lines).block(dash_block(" Devices by status ", theme)),
        top[0],
    );

    // Top-utilized prefixes.
    let bar_w = top[1].width.saturating_sub(28).clamp(1, 24);
    let prefix_lines: Vec<Line> = if data.top_prefixes.is_empty() {
        vec![Line::from(Span::styled(
            "no utilization data",
            Style::default().fg(theme.text_dim),
        ))]
    } else {
        data.top_prefixes
            .iter()
            .map(|(cidr, pct)| {
                let mut spans = vec![Span::styled(
                    format!("{cidr:<20}"),
                    Style::default().fg(theme.text),
                )];
                spans.extend(util_bar(*pct, bar_w, theme));
                Line::from(spans)
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(prefix_lines).block(dash_block(" Top-utilized prefixes ", theme)),
        top[1],
    );

    // Recent activity.
    let activity_lines: Vec<Line> = if data.recent.is_empty() {
        vec![Line::from(Span::styled(
            "no recent journal entries",
            Style::default().fg(theme.text_dim),
        ))]
    } else {
        data.recent
            .iter()
            .map(|j| {
                Line::from(vec![
                    Span::styled(
                        format!("{:<17} ", short_time(&j.created)),
                        Style::default().fg(theme.text_dim),
                    ),
                    Span::styled(format!("{:<9}", j.kind), Style::default().fg(theme.accent)),
                    Span::styled(j.summary.clone(), Style::default().fg(theme.text)),
                ])
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(activity_lines).block(dash_block(" Recent activity ", theme)),
        rows[1],
    );
}

/// For tree rows in display order (depths only), mark each as the last of its
/// siblings: `true` when no later row at the same depth appears before a shallower
/// one. Drives `└` vs `├` and whether an ancestor column draws a continuing `│`.
/// Pure (one backward pass). Callers pass one VRF group at a time so siblings
/// never cross VRF boundaries.
fn last_sibling_flags(depths: &[u64]) -> Vec<bool> {
    let mut flags = vec![true; depths.len()];
    // seen[d] = a row at depth d has been seen *later* within the current context.
    let mut seen: Vec<bool> = Vec::new();
    for i in (0..depths.len()).rev() {
        let d = depths[i] as usize;
        if seen.len() < d + 1 {
            seen.resize(d + 1, false);
        }
        // A shallower/equal row closes any deeper subtrees opened after it.
        for s in seen.iter_mut().skip(d + 1) {
            *s = false;
        }
        flags[i] = !seen[d];
        seen[d] = true;
    }
    flags
}

/// The branch prefix for a tree row: the ancestor pipe columns (`│  ` / `   `)
/// then this row's connector + disclosure. `anc[l]` is the last-sibling flag of
/// the ancestor at depth `l` (last → blank column, else a continuing `│`). At
/// depth 0 there is no connector, only the disclosure. Pure.
fn tree_branch(
    depth: usize,
    anc: &[bool],
    is_last: bool,
    collapsible: bool,
    collapsed: bool,
) -> String {
    let disc = if collapsible {
        if collapsed { "▸" } else { "▾" }
    } else if depth == 0 {
        " "
    } else {
        "─"
    };
    let mut s = String::new();
    for l in 1..depth {
        s.push_str(if anc.get(l).copied().unwrap_or(true) {
            "   "
        } else {
            "│  "
        });
    }
    if depth >= 1 {
        s.push(if is_last { '└' } else { '├' });
    }
    s.push_str(disc);
    s.push(' ');
    s
}

/// A compact `███░░░░░░░ 92%` utilization cell, colored by severity, for a tree
/// row. Ten cells so low single-digit/teens percentages still show a block.
fn tree_util(pct: u8, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(util_bar(pct, 10, theme));
    spans
}

/// Build the prefix-tree body as styled lines: a dim VRF header wherever the VRF
/// changes, then each visible prefix with tree connectors (`├ └ │`), a disclosure
/// marker, status, child count, and a small utilization bar. Returns the lines
/// plus the display-row index of the selected node (for scroll-to-selection).
/// Pure (no widgets/terminal), so the connectors + markers are unit-testable.
fn prefix_tree_lines<'a>(
    data: &'a PrefixTreeData,
    collapsed: &HashSet<u64>,
    selected: usize,
    theme: &Theme,
) -> (Vec<Line<'a>>, usize) {
    let prefix_color = kind_accent("prefix", theme);
    let visible = prefix_tree::visible_indices(&data.nodes, collapsed);
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_row = 0;

    // Walk the visible nodes in contiguous VRF groups so the connector math (which
    // siblings continue) never bleeds across tables.
    let mut g = 0;
    while g < visible.len() {
        let vrf = data.nodes[visible[g]].vrf.as_deref();
        let mut end = g;
        while end < visible.len() && data.nodes[visible[end]].vrf.as_deref() == vrf {
            end += 1;
        }
        let group = &visible[g..end];

        // VRF section header (global first); a blank spacer between groups.
        if g > 0 {
            lines.push(Line::from(""));
        }
        let label = vrf.map_or_else(|| "global table".to_string(), |v| format!("vrf: {v}"));
        lines.push(Line::from(Span::styled(
            format!("  {label}"),
            Style::default()
                .fg(theme.header)
                .add_modifier(Modifier::BOLD),
        )));

        let depths: Vec<u64> = group.iter().map(|&i| data.nodes[i].depth).collect();
        let last_flags = last_sibling_flags(&depths);
        let mut anc: Vec<bool> = Vec::new();
        for (k, &node_idx) in group.iter().enumerate() {
            let node = &data.nodes[node_idx];
            let vis_i = g + k;
            let is_sel = vis_i == selected;
            if is_sel {
                sel_row = lines.len();
            }
            let depth = node.depth as usize;
            let collapsed_here = node.collapsible() && collapsed.contains(&node.id);
            let is_last = last_flags[k];

            anc.truncate(depth);
            let branch = tree_branch(depth, &anc, is_last, node.collapsible(), collapsed_here);
            anc.push(is_last);

            let gutter = if is_sel { "▌ " } else { "  " };
            let cidr_style = {
                let s = Style::default().fg(prefix_color);
                if is_sel {
                    s.add_modifier(Modifier::BOLD)
                } else {
                    s
                }
            };
            let mut spans = vec![
                Span::styled(gutter, Style::default().fg(theme.accent)),
                Span::styled(branch, Style::default().fg(theme.text_dim)),
                Span::styled(node.prefix.clone(), cidr_style),
            ];
            if let Some(status) = &node.status {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(status.clone(), theme.status_style(status)));
            }
            if node.children > 0 {
                spans.push(Span::styled(
                    format!("  [{}]", node.children),
                    Style::default().fg(theme.text_dim),
                ));
            }
            if let Some(pct) = node.utilization {
                spans.extend(tree_util(pct, theme));
            }
            if !node.description.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", node.description),
                    Style::default().fg(theme.text_dim),
                ));
            }
            lines.push(Line::from(spans));
        }
        g = end;
    }
    (lines, sel_row)
}

/// The hierarchical prefix tree (`T`): the IPAM prefix hierarchy, VRF-grouped and
/// depth-indented, with collapse/expand. Read-only; falls back to a
/// loading/error/empty line.
fn render_prefix_tree(frame: &mut Frame, area: Rect, app: &mut App) {
    // Stash the inner height (rows inside the borders + padding) for paging.
    let inner_h = area.height.saturating_sub(2);
    app.sync_tree_viewport(inner_h);
    let theme = &app.theme;

    let Some(data) = app.prefix_tree.as_ref() else {
        let msg = app.prefix_tree_error.as_deref().map_or_else(
            || "Loading prefixes…".to_string(),
            |e| format!("prefix tree error: {e}"),
        );
        frame.render_widget(
            Paragraph::new(msg)
                .block(dash_block(" Prefix tree ", theme))
                .style(Style::default().fg(theme.text_dim)),
            area,
        );
        return;
    };

    if data.nodes.is_empty() {
        frame.render_widget(
            Paragraph::new("No prefixes found.")
                .block(dash_block(" Prefix tree ", theme))
                .style(Style::default().fg(theme.text_dim)),
            area,
        );
        return;
    }

    let visible_len = prefix_tree::visible_indices(&data.nodes, &app.prefix_tree_collapsed).len();
    let (lines, sel_row) = prefix_tree_lines(
        data,
        &app.prefix_tree_collapsed,
        app.prefix_tree_selected,
        theme,
    );

    // Title: how many prefixes are shown (and whether the listing is capped), with
    // a right-aligned cursor position over the visible rows.
    let capped = if data.capped() {
        format!(" (capped at {})", data.nodes.len())
    } else {
        String::new()
    };
    let title = format!(" Prefix tree — {} prefixes{capped} ", data.total);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(1));
    if let Some(pos) = list_position(app.prefix_tree_selected, visible_len) {
        block = block.title(Line::from(pos).right_aligned().style(theme.text_dim));
    }

    // Scroll so the selected row stays visible: pin it to the bottom edge once it
    // scrolls past, clamped so the last page doesn't leave a trailing gap.
    let height = inner_h.max(1) as usize;
    let max_offset = lines.len().saturating_sub(height);
    let offset = sel_row
        .saturating_sub(height.saturating_sub(1))
        .min(max_offset);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0)),
        area,
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
/// A scannable color for a result's kind tag, grouped by NetBox domain so the
/// KIND column reads at a glance (hosts vs addressing vs locations vs circuits vs
/// tenancy). Falls back to dim text for anything unmapped; under `NO_COLOR` every
/// theme color is `Reset`, so this stays uncolored too. Pure + testable.
fn kind_accent(kind: &str, theme: &Theme) -> Color {
    match kind {
        "device" | "vm" | "cluster" => theme.accent,
        "ip" | "prefix" | "aggregate" | "ip-range" | "ip_range" | "asn" | "vlan" => {
            theme.graph_secondary
        }
        "site" | "rack" => theme.header,
        "circuit" | "provider" => theme.warning,
        "tenant" | "contact" => theme.graph_primary,
        _ => theme.text_dim,
    }
}

/// order: KIND / DISPLAY / SITE. The kind tag is colored by domain (see
/// [`kind_accent`]) so the column is scannable; the SITE cell is colored via
/// [`Theme::status_style`] when its value reads like a status (so a status
/// surfaced through the subtitle keeps T4's palette) and stays neutral text
/// otherwise; the display is plain. Pure (no widgets), so the cell text + color
/// decisions are unit-testable.
fn result_row_styled(
    result: &crate::netbox::search::SearchResult,
    theme: &Theme,
) -> [(String, Style); 3] {
    let [kind, display, site] = result_row_cells(result);
    let kind_style = Style::default().fg(kind_accent(&kind, theme));
    let site_style = theme.status_style(&site);
    [
        (kind, kind_style),
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
        .highlight_symbol("▌ ")
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
            ("f / F", "filter / clear"),
            ("Tab / S-Tab", "switch pane / detail tabs"),
            ("j / k", "move / scroll"),
            ("g / G", "top / bottom"),
            ("PgUp / PgDn", "page up / down"),
            ("Enter", "open detail"),
        ],
        // Actions / detail tabs / app.
        vec![
            ("o", "open in browser"),
            ("y", "copy"),
            ("R", "related objects"),
            ("t", "cycle theme"),
            ("r", "refresh"),
            ("D", "dashboard"),
            ("T", "prefix tree"),
            ("P / C-P", "switch profile"),
            ("S", "config / profiles"),
            ("u", "dismiss update"),
            ("i p c v s", "device tabs"),
            ("e", "rack elevation"),
            ("b / Esc", "back / clear search"),
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

/// The related-objects modal's help line (also drives its min width).
const RELATED_HELP: &str = "↑/↓ select · Enter open · Esc close";

/// Build the related-objects pick-list as styled lines: `▌ relation  label` per
/// link, the selected row's relation bolded and the label in its kind color. Pure
/// (no widgets), so the selection marker + coloring are unit-testable.
fn related_lines<'a>(modal: &'a RelatedModal, theme: &Theme) -> Vec<Line<'a>> {
    modal
        .links
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let selected = i == modal.selected;
            let marker = if selected { "▌ " } else { "  " };
            let rel_style = if selected {
                Style::default()
                    .fg(theme.header)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_dim)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.accent)),
                Span::styled(format!("{:<13}", l.relation), rel_style),
                Span::styled(
                    l.label.clone(),
                    Style::default().fg(kind_accent(l.kind.as_str(), theme)),
                ),
            ])
        })
        .collect()
}

/// Render the centered `R` related-objects modal: a pick-list of the current
/// detail's navigable relations (`relation  label`, the label in its kind color),
/// the selected row marked with the `▌` gutter. `Enter` jumps; `Esc` closes.
fn render_related(frame: &mut Frame, area: Rect, modal: &RelatedModal, theme: &Theme) {
    let content_w = modal
        .links
        .iter()
        .map(|l| l.relation.len() + 2 + l.label.chars().count())
        .max()
        .unwrap_or(0)
        .max(RELATED_HELP.chars().count());
    let content_w = u16::try_from(content_w).unwrap_or(40);
    let rows = u16::try_from(modal.links.len()).unwrap_or(1).max(1);
    let popup = centered_popup(area, content_w, rows + 1);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Related objects ")
        .title(
            Line::from(" Esc: close ")
                .right_aligned()
                .style(theme.text_dim),
        )
        .border_style(Style::default().fg(theme.border_focused));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    frame.render_widget(Paragraph::new(related_lines(modal, theme)), areas[0]);
    frame.render_widget(
        Paragraph::new(Span::styled(
            RELATED_HELP,
            Style::default().fg(theme.text_dim),
        )),
        areas[1],
    );
}

/// Render the centered `f` filter modal: a small form over the active filters.
/// The four scope filters collapse into a `scope type` cycle + a `scope value`
/// row; the rest are text fields. `Enter` applies, `Esc` cancels.
fn render_filter(
    frame: &mut Frame,
    area: Rect,
    modal: &mut crate::tui::filter_modal::FilterModal,
    theme: &Theme,
) {
    use crate::tui::filter_modal::row;

    let popup = centered_popup(area, 54, 9);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Filters ")
        .title(
            Line::from(" Enter apply · Esc cancel ")
                .right_aligned()
                .style(Style::default().fg(theme.text_dim)),
        )
        .border_style(Style::default().fg(theme.border_focused))
        .padding(Padding::horizontal(1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut constraints = vec![Constraint::Length(1); row::COUNT];
    constraints.push(Constraint::Length(1)); // blank
    constraints.push(Constraint::Min(1)); // help
    let rows = Layout::vertical(constraints).split(inner);

    let focus = modal.focus;
    let scope_label = modal.scope_type_label();
    let labels = [
        "status",
        "scope type",
        "scope value",
        "tenant",
        "role",
        "tag",
        "vrf",
    ];
    let label_w = 14u16.min(inner.width);
    let mut cursor: Option<Position> = None;

    for i in 0..row::COUNT {
        let r = rows[i];
        let focused = i == focus;
        let marker = if focused { "> " } else { "  " };
        let label_style = Style::default().fg(if focused {
            theme.header
        } else {
            theme.text_dim
        });
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("{marker}{:<12}", labels[i]),
                label_style,
            )),
            Rect::new(r.x, r.y, label_w, 1),
        );
        let value_area = Rect::new(
            r.x.saturating_add(label_w),
            r.y,
            r.width.saturating_sub(label_w),
            1,
        );
        if i == row::SCOPE_TYPE {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("‹ {scope_label} ›"),
                        Style::default().fg(theme.accent),
                    ),
                    Span::styled("  (←/→)", Style::default().fg(theme.text_dim)),
                ])),
                value_area,
            );
        } else if let Some(input) = modal.input_mut(i) {
            let pos = input.render_with_focus(frame, value_area, ' ', theme, focused);
            if focused {
                cursor = Some(pos);
            }
        }
    }
    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }

    frame.render_widget(
        Paragraph::new(Span::styled(
            "↑/↓ field · ←/→ scope · Enter apply · Esc cancel",
            Style::default().fg(theme.text_dim),
        )),
        rows[row::COUNT + 1],
    );
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

    // Size to content, capped to the screen — the ttl/xfr modal idiom
    // (`base_height.min(area.height - 4)`), so the modal floats as a centered box
    // instead of filling the screen like a page. `CONTENT_H` covers the tallest
    // section (the profile add/edit form); on a standard 80x24 it's unchanged.
    const CONTENT_H: u16 = 20;
    let popup = centered_popup(area, 60, CONTENT_H.min(area.height.saturating_sub(4)));
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
    use crate::tui::config_modal::{SETTINGS_CATEGORIES, SettingId, SettingsFocus};

    // Body (two columns: categories | fields) + a message line + a help line.
    let rows = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let cols = Layout::horizontal([Constraint::Length(16), Constraint::Min(1)]).split(rows[0]);
    let (cat_area, field_area) = (cols[0], cols[1]);

    let s = &mut modal.settings;
    // Copy the immutable bits out first so the field loop can borrow inputs mutably.
    let focus = s.focus;
    let selected_cat = s.category;
    let selected_field = s.field;
    let theme_name = s.theme_name();
    let message = s.message.clone();
    let fields = s.current_fields();
    let cats_focused = focus == SettingsFocus::Categories;

    // Left column: the category list (an accent gutter marks the selection).
    let cat_lines: Vec<Line> = SETTINGS_CATEGORIES
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let selected = i == selected_cat;
            let marker = if selected { "▌ " } else { "  " };
            let style = if selected && cats_focused {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else if selected {
                Style::default().fg(theme.header)
            } else {
                Style::default().fg(theme.text_dim)
            };
            Line::from(Span::styled(format!("{marker}{name}"), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(cat_lines), cat_area);

    // Right column: the selected category's fields.
    let label_w = 16u16.min(field_area.width);
    let mut cursor: Option<Position> = None;
    for (i, &id) in fields.iter().enumerate() {
        let y = field_area
            .y
            .saturating_add(u16::try_from(i).unwrap_or(u16::MAX));
        if y >= field_area.bottom() {
            break;
        }
        let focused = focus == SettingsFocus::Fields && i == selected_field;
        let label_style = Style::default().fg(if focused {
            theme.header
        } else {
            theme.text_dim
        });
        let marker = if focused { "> " } else { "  " };
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("{marker}{:<14}", id.label()),
                label_style,
            )),
            Rect::new(field_area.x, y, label_w, 1),
        );
        let value_area = Rect::new(
            field_area.x.saturating_add(label_w),
            y,
            field_area.width.saturating_sub(label_w),
            1,
        );
        if id == SettingId::Theme {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(theme_name, Style::default().fg(theme.accent)),
                    Span::styled("  (←/→/Space)", Style::default().fg(theme.text_dim)),
                ])),
                value_area,
            );
        } else if id == SettingId::CacheEnabled {
            let on = s.cache_enabled();
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if on { "on" } else { "off" },
                        Style::default().fg(if on { theme.accent } else { theme.text_dim }),
                    ),
                    Span::styled("  (←/→/Space)", Style::default().fg(theme.text_dim)),
                ])),
                value_area,
            );
        } else if let Some(input) = s.input_mut(id) {
            let pos = input.render_with_focus(frame, value_area, ' ', theme, focused);
            if focused {
                cursor = Some(pos);
            }
        }
    }
    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }

    // Message line (validation / info).
    if let Some(msg) = &message {
        frame.render_widget(
            Paragraph::new(Span::styled(msg.clone(), Style::default().fg(theme.error))),
            rows[1],
        );
    }

    // Help line, context-sensitive to the focused pane.
    let help = if cats_focused {
        "↑/↓ category   → fields   Enter save   Tab section   Esc close"
    } else {
        "↑/↓ field   Esc back   Enter/Ctrl+S save   Tab section"
    };
    frame.render_widget(
        Paragraph::new(Span::styled(help, Style::default().fg(theme.text_dim))),
        rows[2],
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

    // Render the nav into the shared inset (same gutter as the editor) so it lines
    // up with the header/panes instead of hugging the left edge; the bg block above
    // already filled the full row edge-to-edge.
    let line = footer_line(app);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(app.theme.text).bg(app.theme.chrome_bg)),
        footer_input_area(area),
    );
}

/// Inset a footer rect by two columns on each side: the shared left gutter for
/// all the chrome (header, filter bar, footer nav, and the Search/Command editor)
/// so they line up with the bordered panes' content column (border + one pad) —
/// nothing hugs the terminal edge. Width floors at 0 on a tiny terminal.
fn footer_input_area(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(2),
        y: area.y,
        width: area.width.saturating_sub(4),
        height: area.height,
    }
}

/// Context-sensitive normal-mode footer. Live state (spinner, result count,
/// errors, transient theme notices) gets the left edge; persistent navigation
/// follows so controls stay visible without burying the thing that just changed.
fn footer_line(app: &App) -> Line<'static> {
    let theme = &app.theme;
    let mut spans: Vec<Span> = Vec::new();

    // Always reserve the spinner's two columns so the status/count never shifts
    // when a load toggles the spinner: the glyph while loading, two blanks idle.
    if app.loading() {
        spans.push(app.spinner.span(theme));
        spans.push(Span::raw(" "));
    } else {
        spans.push(Span::raw("  "));
    }

    let mut has_text = false;
    if !app.status.is_empty() {
        spans.push(Span::styled(
            app.status.clone(),
            theme.message_style(app.status_severity),
        ));
        has_text = true;
    } else if app.loading() {
        spans.push(Span::styled(
            "loading…",
            Style::default().fg(theme.text_dim),
        ));
        has_text = true;
    }

    // On the detail screen, show how old a cache-served object is ("cached Ns
    // ago"). A freshly-fetched object (`Origin`) shows nothing — it's current.
    if app.screen == Screen::Detail
        && let Some(f) = app.detail_freshness
        && f.source == Source::Cache
    {
        if has_text {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("cached {}", fmt_age(f.age)),
            Style::default().fg(theme.text_dim),
        ));
    }

    // Reserve a fixed-width slot for the transient state so its width changing —
    // the spinner flicking on for an instant cache hit, a status appearing — can't
    // shift the navigation that follows. Filler pads a short/empty state to a
    // stable column; a guaranteed ≥2-col gap keeps a longer status off the nav.
    let state_width: usize = spans.iter().map(|s| s.content.width()).sum();
    let pad = STATE_MIN_WIDTH.saturating_sub(state_width).max(2);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(nav_spans(footer_nav(app), theme));
    Line::from(spans)
}

/// Reserved width (columns) for the footer's transient state region, so the nav
/// keeps a stable start column as the spinner/status come and go. Covers the
/// frequent cases (spinner + `loading…`, a result count, the freshness chip); a
/// rarer long status simply pushes the nav right by its overflow.
const STATE_MIN_WIDTH: usize = 18;

/// A compact relative age for the footer freshness chip. The cache TTL caps at
/// five minutes, so seconds and whole minutes cover every case.
fn fmt_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else {
        format!("{}m ago", secs / 60)
    }
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
            " / search · j/k move · Enter open · D dash · T tree · S settings · Tab preview · o/y open/copy · r refresh · t theme · ? help · q quit "
        }
        Screen::Detail => {
            " j/k scroll · Tab/i/p/c/v/s tabs · R related · o/y open/copy · b back · r refresh · ? help · q quit "
        }
        Screen::Dashboard => {
            " r refresh · b/Esc back · T tree · S settings · / search · ? help · q quit "
        }
        Screen::PrefixTree => {
            " j/k move · Space/←/→ collapse/expand · Enter open · o/y open/copy · r refresh · b/Esc back · ? help · q quit "
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
    use crate::netbox::prefix_tree::PrefixNode;

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
    fn footer_input_area_insets_two_columns_each_side() {
        // The shared chrome gutter: nav + search/command editor sit two columns in
        // from each edge, lining up with the bordered panes' content column.
        let full = Rect::new(0, 23, 80, 1);
        let inset = footer_input_area(full);
        assert_eq!(inset.x, 2, "left-padded by two columns");
        assert_eq!(inset.width, 76, "two columns trimmed off each side");
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
    fn kind_accent_groups_kinds_by_domain() {
        let t = Theme::default_theme();
        // Hosts/compute share the accent; addressing shares graph_secondary.
        assert_eq!(kind_accent("device", &t), t.accent);
        assert_eq!(kind_accent("vm", &t), t.accent);
        assert_eq!(kind_accent("prefix", &t), t.graph_secondary);
        assert_eq!(kind_accent("ip", &t), t.graph_secondary);
        // Locations, circuits, tenancy each get their own color.
        assert_eq!(kind_accent("site", &t), t.header);
        assert_eq!(kind_accent("circuit", &t), t.warning);
        assert_eq!(kind_accent("tenant", &t), t.graph_primary);
        // Anything unmapped falls back to dim text.
        assert_eq!(kind_accent("mystery", &t), t.text_dim);
    }

    #[test]
    fn any_filter_active_detects_a_set_filter() {
        let mut f = SearchFilters::default();
        assert!(!any_filter_active(&f), "no filters set");
        f.status = Some("active".into());
        assert!(any_filter_active(&f), "a status filter counts");
        let g = SearchFilters {
            vrf: Some("mgmt".into()),
            ..SearchFilters::default()
        };
        assert!(any_filter_active(&g), "a vrf filter counts");
    }

    fn tree_node(id: u64, cidr: &str, depth: u64, children: u64) -> PrefixNode {
        PrefixNode {
            id,
            prefix: cidr.into(),
            vrf: None,
            status: Some("active".into()),
            depth,
            children,
            utilization: None,
            description: String::new(),
        }
    }

    #[test]
    fn prefix_tree_lines_connect_depth_and_mark_collapsible() {
        let theme = Theme::default_theme();
        let data = PrefixTreeData {
            nodes: vec![
                tree_node(1, "10.0.0.0/8", 0, 1),
                tree_node(2, "10.0.0.0/24", 1, 0),
            ],
            total: 2,
        };
        let (lines, sel_row) = prefix_tree_lines(&data, &HashSet::new(), 0, &theme);
        // A "global table" header, then the two prefix rows.
        assert!(line_text(&lines[0]).contains("global table"));
        assert!(line_text(&lines[1]).contains("10.0.0.0/8"));
        assert!(line_text(&lines[2]).contains("10.0.0.0/24"));
        // The expanded collapsible root shows the open triangle, no branch connector.
        let root_branch = &lines[1].spans[1].content;
        assert!(root_branch.contains('▾'), "expanded root → ▾");
        assert!(
            !root_branch.contains('├') && !root_branch.contains('└'),
            "a depth-0 root has no branch connector"
        );
        // The depth-1 child is drawn with a branch connector (last child → └).
        assert!(
            lines[2].spans[1].content.contains('└'),
            "the only child gets the last-sibling connector └"
        );
        // Selected node (visible index 0) maps to its display row (row 1, after the header).
        assert_eq!(sel_row, 1);
    }

    #[test]
    fn last_sibling_flags_marks_final_child_per_subtree() {
        // 10/8 → {/16 → /24}, /16b ; depths: 0,1,2,1
        let flags = last_sibling_flags(&[0, 1, 2, 1]);
        assert_eq!(flags, vec![true, false, true, true]);
        // The first /16 is NOT last (another /16 follows); the /24 and last /16 are.
    }

    #[test]
    fn prefix_tree_lines_collapsed_marks_and_hides_child() {
        let theme = Theme::default_theme();
        let data = PrefixTreeData {
            nodes: vec![
                tree_node(1, "10.0.0.0/8", 0, 1),
                tree_node(2, "10.0.0.0/24", 1, 0),
            ],
            total: 2,
        };
        let collapsed: HashSet<u64> = [1].into_iter().collect();
        let (lines, _) = prefix_tree_lines(&data, &collapsed, 0, &theme);
        let body: Vec<String> = lines.iter().map(line_text).collect();
        assert!(
            body.iter().any(|l| l.contains('▸')),
            "collapsed → ▸: {body:?}"
        );
        assert!(
            !body.iter().any(|l| l.contains("10.0.0.0/24")),
            "the collapsed child is hidden: {body:?}"
        );
    }

    #[test]
    fn footer_nav_covers_prefix_tree() {
        let mut a = app();
        a.screen = Screen::PrefixTree;
        let nav = footer_nav(&a);
        assert!(
            nav.contains("collapse/expand"),
            "tree footer mentions collapse"
        );
        assert!(nav.contains("Enter open"), "tree footer mentions open");
    }

    #[test]
    fn util_bar_floors_fill_but_keeps_nonzero_visible() {
        let theme = Theme::default_theme();
        // The first span is the filled run; its char count is the cell count.
        let filled = |pct, w| util_bar(pct, w, &theme)[0].content.chars().count();
        assert_eq!(filled(0, 10), 0, "zero → empty");
        assert_eq!(
            filled(5, 10),
            1,
            "a sliver still shows one block, not nothing"
        );
        assert_eq!(filled(10, 10), 1, "10% of 10 → one block");
        assert_eq!(filled(50, 10), 5);
        assert_eq!(filled(99, 10), 9, "99% does not round up to full");
        assert_eq!(filled(100, 10), 10, "only a true 100% fills every cell");
    }

    #[test]
    fn detail_footer_advertises_related_jump() {
        let mut a = app();
        a.screen = Screen::Detail;
        assert!(
            footer_nav(&a).contains("R related"),
            "detail footer surfaces R"
        );
    }

    #[test]
    fn related_lines_mark_selection_and_color_by_kind() {
        use crate::domain::detail::ObjectLink;
        use crate::netbox::search::ObjectKind;
        let theme = Theme::default_theme();
        let modal = RelatedModal {
            links: vec![
                ObjectLink {
                    kind: ObjectKind::Site,
                    id: 5,
                    relation: "site".into(),
                    label: "iad1".into(),
                },
                ObjectLink {
                    kind: ObjectKind::Rack,
                    id: 7,
                    relation: "rack".into(),
                    label: "R1".into(),
                },
            ],
            selected: 1,
        };
        let lines = related_lines(&modal, &theme);
        assert_eq!(lines.len(), 2);
        // Unselected row gets the blank gutter; the selected row gets `▌`.
        assert!(line_text(&lines[0]).starts_with("  site"));
        assert!(line_text(&lines[1]).starts_with("▌ rack"));
        // The label is colored by its kind (rack groups with the dcim color).
        assert_eq!(last_fg(&lines[1]), Some(kind_accent("rack", &theme)));
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
            text.trim_start().starts_with("theme: nord"),
            "status leads (after the reserved spinner slot): {text}"
        );
        assert!(text.contains("/ search"), "nav remains present: {text}");
        let status_idx = text.find("theme: nord").expect("status present");
        let nav_idx = text.find("/ search").expect("nav present");
        assert!(status_idx < nav_idx, "status precedes navigation: {text}");
    }

    #[test]
    fn footer_shows_cached_age_on_detail() {
        use crate::cache::{Freshness, Source};
        let mut a = app();
        a.screen = Screen::Detail;
        a.detail_freshness = Some(Freshness {
            source: Source::Cache,
            age: 42,
        });
        let text = line_text(&footer_line(&a));
        assert!(text.contains("cached 42s ago"), "footer: {text}");
    }

    #[test]
    fn footer_hides_age_for_freshly_fetched_detail() {
        use crate::cache::{Freshness, Source};
        let mut a = app();
        a.screen = Screen::Detail;
        // A just-fetched object is current — no "cached" chip.
        a.detail_freshness = Some(Freshness {
            source: Source::Origin,
            age: 0,
        });
        let text = line_text(&footer_line(&a));
        assert!(
            !text.contains("cached"),
            "origin-fetched shows no age: {text}"
        );
    }

    #[test]
    fn footer_nav_column_is_stable_when_spinner_toggles() {
        // The display column where the nav starts must not move when the spinner
        // appears/disappears — otherwise the footer jitters on instant cache hits.
        let nav_col = |a: &App| -> usize {
            let text = line_text(&footer_line(a));
            let idx = text.find("/ search").expect("home nav present");
            text[..idx].width()
        };
        let mut a = app();
        let idle = nav_col(&a);
        a.pending = 1; // spinner on
        let loading = nav_col(&a);
        assert_eq!(idle, loading, "spinner must not shift the nav column");
        // A short status (within the reserved slot) also keeps the column.
        a.pending = 0;
        a.status = "12 result(s)".into();
        assert_eq!(nav_col(&a), idle, "a short status keeps the nav column");
    }

    #[test]
    fn fmt_age_uses_seconds_then_minutes() {
        assert_eq!(fmt_age(0), "0s ago");
        assert_eq!(fmt_age(45), "45s ago");
        assert_eq!(fmt_age(60), "1m ago");
        assert_eq!(fmt_age(125), "2m ago");
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
        // The kind cell is colored by domain (device → accent); the display uses
        // the table's base text.
        assert_eq!(cells[0].1.fg, Some(theme.accent));
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
