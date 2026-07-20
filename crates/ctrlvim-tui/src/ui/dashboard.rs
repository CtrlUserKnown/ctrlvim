//! The Dashboard buffer: header, keybindings sidebar, and the
//! workspace / settings / about sections.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{Action, App, DashboardSection, Layout as DLayout, PanelId};
use crate::model::KEYBINDINGS;
use crate::theme;

use super::{
    hint_badge, icon_chip, row_style, selection_bar, titled_panel, titled_panel_with_right, Zones,
};

/// charvim logo glyph + wordmark, then the workspace/settings/about tab row.
pub fn header(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    // Row 0: logo mark + wordmark.
    let logo = Line::from(vec![
        Span::styled("▐▛ ", Style::default().fg(theme::BLUE)),
        Span::styled("charvim", Style::default().fg(theme::FG).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(logo), Rect { height: 1, ..area });

    // Row 2: section tabs with keybinding hints. Skip if there isn't room
    // (keeps rendering safe at very small terminal sizes).
    let tab_y = area.y + 2;
    if tab_y >= area.y + area.height {
        return;
    }
    let tabs = [
        ('w', "workspace", DashboardSection::Workspace),
        ('s', "settings", DashboardSection::Settings),
        ('a', "about", DashboardSection::About),
    ];
    let right = area.x + area.width; // frame edge; ratatui does not clip for us
    let mut x = area.x;
    let mut underline: Vec<(u16, u16)> = Vec::new(); // (start_x, width) for active
    for (key, label, section) in tabs {
        if x >= right {
            break;
        }
        let active = app.section == section;
        let color = if active { theme::FG } else { theme::FG_DIM };
        let start = x;
        let seg = Line::from(vec![
            hint_badge(key),
            Span::raw(" "),
            Span::styled(label, Style::default().fg(color)),
        ]);
        let w = (seg.width() as u16).min(right - x);
        f.render_widget(Paragraph::new(seg), Rect { x, y: tab_y, width: w, height: 1 });
        zones.push(Rect { x: start, y: tab_y, width: w, height: 1 }, Action::GotoSection(section));
        if active {
            underline.push((start, w));
        }
        x += w + 3; // gap between tabs
    }

    // Row 3: a full-width dim divider with a blue underline under the active tab.
    let div_y = area.y + 3;
    if div_y < area.y + area.height {
        let divider = "─".repeat(area.width as usize);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(divider, Style::default().fg(theme::BORDER_DIM)))),
            Rect { x: area.x, y: div_y, width: area.width, height: 1 },
        );
        for (sx, w) in underline {
            let w = w.min(right.saturating_sub(sx));
            if w == 0 {
                continue;
            }
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("━".repeat(w as usize), Style::default().fg(theme::BLUE)))),
                Rect { x: sx, y: div_y, width: w, height: 1 },
            );
        }
    }
}

/// The persistent KEYBINDINGS panel shown to the left of the workspace content.
pub fn keybindings_sidebar(f: &mut Frame, area: Rect) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let inner = titled_panel(f, area, "KEYBINDINGS", theme::GREEN);
    let mut lines: Vec<Line> = Vec::new();
    for kb in KEYBINDINGS {
        lines.push(Line::from(Span::styled(kb.keys, Style::default().fg(theme::CYAN))));
        lines.push(Line::from(Span::styled(kb.desc, Style::default().fg(theme::FG_DIM))));
        lines.push(Line::raw(""));
    }
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::BG_DARK)),
        inner,
    );
}

/// Dispatch to the active dashboard section.
pub fn screen(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    match app.section {
        DashboardSection::Workspace => workspace(f, app, area, zones),
        DashboardSection::Settings => settings(f, app, area, zones),
        DashboardSection::About => about(f, area),
    }
}

fn workspace(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    // Layout switcher row: "DASHBOARD LAYOUT   [2] columns  [3] grid".
    let cols_active = app.layout == DLayout::Columns;
    let pill = |label: &'static str, key: char, active: bool| -> Vec<Span<'static>> {
        let (bg, fg) = if active { (theme::BORDER_DIM, theme::FG) } else { (theme::BG, theme::FG_DIM) };
        vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(format!("[{key}]"), Style::default().fg(theme::FG_DIM).bg(bg)),
            Span::styled(format!(" {label} "), Style::default().fg(fg).bg(bg)),
        ]
    };
    let mut switch_spans = vec![
        Span::styled("DASHBOARD LAYOUT", Style::default().fg(theme::FG_DIM)),
        Span::raw("   "),
    ];
    let cols_x = area.x + switch_spans.iter().map(|s| s.width() as u16).sum::<u16>();
    let cols_pill = pill("columns", '2', cols_active);
    let cols_w: u16 = cols_pill.iter().map(|s| s.width() as u16).sum();
    switch_spans.extend(cols_pill);
    switch_spans.push(Span::raw("  "));
    let grid_x = area.x + switch_spans.iter().map(|s| s.width() as u16).sum::<u16>();
    let grid_pill = pill("grid", '3', !cols_active);
    let grid_w: u16 = grid_pill.iter().map(|s| s.width() as u16).sum();
    switch_spans.extend(grid_pill);
    f.render_widget(Paragraph::new(Line::from(switch_spans)), Rect { height: 1, ..area });
    zones.push(Rect { x: cols_x, y: area.y, width: cols_w, height: 1 }, Action::SetLayout(DLayout::Columns));
    zones.push(Rect { x: grid_x, y: area.y, width: grid_w, height: 1 }, Action::SetLayout(DLayout::Grid));

    let body = Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(2),
        ..area
    };
    if body.height == 0 {
        return;
    }
    if cols_active {
        columns_layout(f, app, body, zones);
    } else {
        grid_layout(f, app, body, zones);
    }
}

// --- columns layout --------------------------------------------------------

fn columns_layout(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .spacing(2)
        .split(area);

    // Left: RECENT FILES (tall).
    let inner = titled_panel(f, cols[0], "RECENT FILES", theme::BLUE);
    recent_files_rows(f, app, inner, zones, true);

    // Right: SESSIONS over STATS.
    let sessions = &app.project.sessions;
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(sessions.len().max(1) as u16 * 2 + 3), Constraint::Min(5)])
        .spacing(1)
        .split(cols[1]);

    let s_inner = titled_panel(f, right[0], "SESSIONS", theme::GREEN);
    let mut s_lines: Vec<Line> = Vec::new();
    for s in sessions {
        s_lines.push(Line::from(vec![
            Span::styled(s.name.clone(), Style::default().fg(theme::FG)),
            Span::raw("  "),
            Span::styled(s.last.clone(), Style::default().fg(theme::FG_DIM)),
        ]));
        s_lines.push(Line::from(Span::styled(
            format!("⌸ {} · {} files", s.branch, s.files),
            Style::default().fg(theme::FG_DIM),
        )));
    }
    f.render_widget(Paragraph::new(s_lines).style(Style::default().bg(theme::BG_DARK)), s_inner);

    let st_inner = titled_panel(f, right[1], "STATS", theme::ORANGE);
    let stats_data = &app.project.stats;
    let stat = |k: &'static str, v: String, c| {
        Line::from(vec![
            Span::styled(format!("{k:<9}"), Style::default().fg(theme::FG_DIM)),
            Span::styled(v, Style::default().fg(c)),
        ])
    };
    let stats = vec![
        stat("startup", format!("{}ms", stats_data.startup_ms), theme::GREEN),
        stat("plugins", format!("{}/{}", stats_data.plugins_loaded, stats_data.plugins_total), theme::CYAN),
        stat("loc", stats_data.loc.clone(), theme::PURPLE),
    ];
    f.render_widget(Paragraph::new(stats).style(Style::default().bg(theme::BG_DARK)), st_inner);
}

/// Render the recent-files list (icon chip + name/path + modified), with the
/// keyboard-focused row highlighted. `full_path` picks path vs. name.
fn recent_files_rows(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones, full_path: bool) {
    if app.project.recent_files.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("no files", Style::default().fg(theme::FG_DIM))))
                .style(Style::default().bg(theme::BG_DARK)),
            area,
        );
        return;
    }
    for (i, file) in app.project.recent_files.iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let selected = i == app.file_index;
        let y = area.y + i as u16;
        let row = Rect { x: area.x, y, width: area.width, height: 1 };
        f.render_widget(Block::default().style(row_style(selected)), row);
        let label = if full_path { file.path.as_str() } else { file.name.as_str() };
        let mut spans = vec![
            selection_bar(selected, theme::BLUE),
            icon_chip(file.icon_letter, file.icon_color),
            Span::raw(" "),
            Span::styled(format!("{label} "), Style::default().fg(theme::FG)),
        ];
        // Right-aligned modified timestamp.
        let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
        let mod_w = file.modified.chars().count() as u16;
        if area.width > used + mod_w {
            spans.push(Span::styled(" ".repeat((area.width - used - mod_w) as usize), row_style(selected)));
        }
        spans.push(Span::styled(file.modified.clone(), Style::default().fg(theme::FG_DIM)));
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(row_style(selected)),
            row,
        );
        zones.push(row, Action::OpenFile(i));
    }
}

// --- grid / bento layout ---------------------------------------------------

#[derive(Clone, Copy)]
enum GridPanel {
    RecentFiles,
    Git,
    Plugins,
}

fn grid_layout(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    let panels = [
        (GridPanel::RecentFiles, PanelId::RecentFiles, app.expand_recent_files),
        (GridPanel::Git, PanelId::Git, app.expand_git),
        (GridPanel::Plugins, PanelId::Plugins, app.expand_plugins),
    ];

    // Greedy row packing: an expanded panel takes a full row; otherwise two
    // collapsed panels share a row.
    let mut rows: Vec<Vec<(GridPanel, PanelId)>> = Vec::new();
    let mut i = 0;
    while i < panels.len() {
        let (gp, pid, expanded) = panels[i];
        if expanded {
            rows.push(vec![(gp, pid)]);
            i += 1;
        } else if i + 1 < panels.len() && !panels[i + 1].2 {
            rows.push(vec![(gp, pid), (panels[i + 1].0, panels[i + 1].1)]);
            i += 2;
        } else {
            rows.push(vec![(gp, pid)]);
            i += 1;
        }
    }

    let mut y = area.y;
    for row in rows {
        let h = row
            .iter()
            .map(|(gp, _)| panel_height(app, *gp))
            .max()
            .unwrap_or(6)
            .min(area.height.saturating_sub(y - area.y));
        if h < 3 {
            break;
        }
        let full = row.len() == 1 && app.panel_expanded(row[0].1);
        if row.len() == 2 {
            let cells = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(2)
                .split(Rect { x: area.x, y, width: area.width, height: h });
            grid_panel(f, app, row[0].0, row[0].1, cells[0], zones);
            grid_panel(f, app, row[1].0, row[1].1, cells[1], zones);
        } else {
            let w = if full { area.width } else { area.width / 2 };
            grid_panel(f, app, row[0].0, row[0].1, Rect { x: area.x, y, width: w, height: h }, zones);
        }
        y += h + 1;
    }
}

fn panel_height(app: &App, p: GridPanel) -> u16 {
    match p {
        GridPanel::RecentFiles => app.project.recent_files.len().max(1) as u16 + 3,
        GridPanel::Git => (if app.expand_git { 7 } else { 4 }) + 3,
        GridPanel::Plugins => {
            (if app.expand_plugins { app.project.plugins.len().max(1) as u16 + 1 } else { 5 }) + 3
        }
    }
}

fn grid_panel(f: &mut Frame, app: &App, gp: GridPanel, pid: PanelId, area: Rect, zones: &mut Zones) {
    let expanded = app.panel_expanded(pid);
    let icon = if expanded { "⌃" } else { "⌄" };
    let (name, accent, key) = match gp {
        GridPanel::RecentFiles => ("RECENT FILES".to_string(), theme::BLUE, 'r'),
        GridPanel::Git => ("GIT STATUS".to_string(), theme::PURPLE, 'g'),
        GridPanel::Plugins => (
            // header shows the loaded/total count
            format!("PLUGINS ({}/{})", app.project.stats.plugins_loaded, app.project.plugins.len()),
            theme::ORANGE,
            'p',
        ),
    };
    let right = Line::from(vec![Span::styled(format!(" {icon} "), Style::default().fg(theme::FG_DIM))]);
    // Prefix the title with the keybinding hint.
    let title = format!("[{key}] {name}");
    let inner = titled_panel_with_right(f, area, &title, accent, Some(right));

    // Toggle zone over the icon glyph on the top border (right side).
    let toggle = Rect { x: area.x + area.width.saturating_sub(4), y: area.y, width: 3, height: 1 };
    zones.push(toggle, Action::TogglePanel(pid));

    match gp {
        GridPanel::RecentFiles => recent_files_rows(f, app, inner, zones, false),
        GridPanel::Git => git_rows(f, app, inner),
        GridPanel::Plugins => plugins_rows(f, app, inner, zones),
    }
}

fn git_rows(f: &mut Frame, app: &App, area: Rect) {
    let Some(g) = &app.project.git else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("not a git repository", Style::default().fg(theme::FG_DIM))))
                .style(Style::default().bg(theme::BG_DARK)),
            area,
        );
        return;
    };
    let kv = |k: &'static str, val: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled(format!("{k:<16}"), Style::default().fg(theme::FG_DIM))];
        spans.extend(val);
        Line::from(spans)
    };
    let mut lines = vec![
        kv("branch", vec![Span::styled(format!(" {}", g.branch), Style::default().fg(theme::PURPLE))]),
        kv("ahead / behind", vec![
            Span::styled(format!("↑{}", g.ahead), Style::default().fg(theme::GREEN)),
            Span::raw(" "),
            Span::styled(format!("↓{}", g.behind), Style::default().fg(theme::RED)),
        ]),
        kv("modified", vec![Span::styled(g.modified.to_string(), Style::default().fg(theme::ORANGE))]),
        kv("staged", vec![Span::styled(g.staged.to_string(), Style::default().fg(theme::CYAN))]),
    ];
    if app.expand_git {
        let remote = if g.remote.is_empty() { "—".to_string() } else { g.remote.clone() };
        lines.push(kv("remote", vec![Span::styled(remote, Style::default().fg(theme::FG_MUTED))]));
        lines.push(kv("last commit", vec![Span::styled(g.last_commit.clone(), Style::default().fg(theme::FG_MUTED))]));
        lines.push(kv("untracked", vec![Span::styled(g.untracked.to_string(), Style::default().fg(theme::RED))]));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::BG_DARK)), area);
}

fn plugins_rows(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    if app.project.plugins.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("no plugins installed", Style::default().fg(theme::FG_DIM))))
                .style(Style::default().bg(theme::BG_DARK)),
            area,
        );
        return;
    }
    let count = if app.expand_plugins { app.project.plugins.len() } else { 4 };
    let mut lines: Vec<Line> = Vec::new();
    for p in app.project.plugins.iter().take(count) {
        let used = p.name.chars().count() as u16;
        let label = p.status.label();
        let lw = label.chars().count() as u16;
        let mut spans = vec![Span::styled(p.name.clone(), Style::default().fg(theme::FG))];
        if area.width > used + lw {
            spans.push(Span::raw(" ".repeat((area.width - used - lw) as usize)));
        } else {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(label, Style::default().fg(p.status.color())));
        lines.push(Line::from(spans));
    }
    let link = if app.expand_plugins { "open plugin manager →" } else { "view all →" };
    // Row index of the link (plugin rows, then a blank spacer line).
    let link_y = area.y + lines.len() as u16 + 1;
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(link, Style::default().fg(theme::FG_DIM))));
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::BG_DARK)), area);

    // Clicking the link opens the plugin manager (expanded) or expands (collapsed).
    let link_rect = Rect { x: area.x, y: link_y, width: area.width, height: 1 };
    let action = if app.expand_plugins { Action::OpenPlugins } else { Action::TogglePanel(PanelId::Plugins) };
    zones.push(link_rect, action);
}

// --- settings (LSP) --------------------------------------------------------

fn settings(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    if area.width < 10 || area.height < 4 {
        return;
    }
    // Header row.
    let header = Line::from(vec![
        Span::styled("LANGUAGE SERVERS", Style::default().fg(theme::FG_DIM)),
    ]);
    f.render_widget(Paragraph::new(header), Rect { height: 1, ..area });
    let active = format!("{} active", app.lsp_active_count());
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(active, Style::default().fg(theme::FG_DIM))).right_aligned()),
        Rect { height: 1, ..area },
    );

    // Size the table to its content (header + rows + borders) so the footer
    // sits cleanly beneath it rather than on the bottom border.
    let table_h = (app.project.lsp.len() as u16 + 3).min(area.height.saturating_sub(2));
    let table = Rect { y: area.y + 2, height: table_h, ..area };
    let inner = super::titled_panel(f, table, "LSP", theme::ORANGE);

    // Column header.
    let ch = Line::from(vec![
        Span::styled(format!("{:<18}", "SERVER"), Style::default().fg(theme::FG_DIM).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<20}", "FILETYPES"), Style::default().fg(theme::FG_DIM).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<12}", "STATUS"), Style::default().fg(theme::FG_DIM).add_modifier(Modifier::BOLD)),
    ]);
    if inner.height > 0 {
        f.render_widget(Paragraph::new(ch).style(Style::default().bg(theme::BG_DARK)), Rect { height: 1, ..inner });
    }

    for (i, lsp) in app.project.lsp.iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let on = app.lsp_enabled[i];
        let selected = i == app.lsp_index;
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Block::default().style(row_style(selected)), row);

        // Status reflects PATH detection + the on/off toggle.
        let (status_label, status_color) = if !lsp.installed {
            ("not found", theme::RED)
        } else if on {
            ("running", theme::GREEN)
        } else {
            ("disabled", theme::FG_DIM)
        };
        let toggle = if on { "●on " } else { "○off" };
        let toggle_color = if on { theme::GREEN } else { theme::FG_DIM };
        let spans = vec![
            selection_bar(selected, theme::BLUE),
            Span::styled(format!("{:<18}", lsp.name), Style::default().fg(theme::FG)),
            Span::styled(format!("{:<20}", lsp.filetypes), Style::default().fg(theme::FG_DIM)),
            Span::styled(format!("{:<12}", status_label), Style::default().fg(status_color)),
            Span::styled(toggle, Style::default().fg(toggle_color)),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
        zones.push(row, Action::SetLspIndex(i));
        // The toggle glyph itself flips the server.
        let toggle_w = 4u16;
        let tx = row.x + row.width.saturating_sub(toggle_w);
        zones.push(Rect { x: tx, y, width: toggle_w, height: 1 }, Action::ToggleLsp(i));
    }

    // Footer config path, one blank line beneath the table.
    let foot_y = table.y + table_h + 1;
    if foot_y < area.y + area.height {
        let footer = Line::from(vec![
            Span::styled("Config file: ", Style::default().fg(theme::FG_DIM)),
            Span::styled("~/.config/ctrlvim/lsp.toml", Style::default().fg(theme::CYAN)),
        ]);
        f.render_widget(Paragraph::new(footer), Rect { x: area.x, y: foot_y, width: area.width, height: 1 });
    }
}

// --- about -----------------------------------------------------------------

fn about(f: &mut Frame, area: Rect) {
    if area.width < 10 || area.height < 6 {
        return;
    }
    let block_w = 40u16.min(area.width);
    let cx = area.x + (area.width.saturating_sub(block_w)) / 2;
    let bottom = area.y + area.height;
    let mut y = area.y + 2;
    // Render one centered line only if it fits vertically.
    macro_rules! centered_at {
        ($yy:expr, $line:expr) => {
            if $yy < bottom {
                f.render_widget(
                    Paragraph::new($line.centered()),
                    Rect { x: area.x, y: $yy, width: area.width, height: 1 },
                );
            }
        };
    }

    let big_logo = ["  ▟█▙  ", " ▟█ █▙ ", "▟█   █▙"];
    for l in big_logo {
        centered_at!(y, Line::from(Span::styled(l, Style::default().fg(theme::BLUE))));
        y += 1;
    }
    y += 1;
    centered_at!(y, Line::from(Span::styled("charvim", Style::default().fg(theme::FG).add_modifier(Modifier::BOLD))));
    y += 1;
    centered_at!(y, Line::from(Span::styled("a rust tui editor · v0.4.2", Style::default().fg(theme::FG_DIM))));
    y += 2;

    let kv = |k: &'static str, v: &'static str, vc| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), Style::default().fg(theme::FG_DIM)),
            Span::styled(v, Style::default().fg(vc)),
        ])
    };
    let rows = [
        kv("rust", "1.82.0", theme::FG),
        kv("ratatui", "0.29", theme::FG),
        kv("crossterm", "0.28", theme::FG),
        kv("license", "MIT", theme::FG),
        kv("repo", "github.com/charvim/charvim", theme::CYAN),
    ];
    for line in rows {
        if y >= area.y + area.height {
            break;
        }
        f.render_widget(Paragraph::new(line), Rect { x: cx, y, width: block_w, height: 1 });
        y += 1;
    }
}
