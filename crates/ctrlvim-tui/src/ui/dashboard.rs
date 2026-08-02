//! The Dashboard buffer: header and the workspace / settings / about sections.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{Action, App, DashboardSection};
use crate::theme;

use super::{file_chip, hint_badge, icon_chip, row_style, selection_bar, titled_panel, Zones};

/// ctrlvim logo glyph + wordmark, then the workspace/settings/about tab row.
pub fn header(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    // Row 0: logo mark + wordmark.
    let logo = Line::from(vec![
        Span::styled("▐▛ ", Style::default().fg(theme::blue())),
        Span::styled("ctrlvim", Style::default().fg(theme::fg()).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(logo), Rect { height: 1, ..area });

    // Row 2: section tabs with keybinding hints. Skip if there isn't room
    // (keeps rendering safe at very small terminal sizes).
    let tab_y = area.y + 2;
    if tab_y >= area.y + area.height {
        return;
    }
    let tabs = [
        ('1', "workspace", DashboardSection::Workspace),
        ('2', "settings", DashboardSection::Settings),
        ('3', "about", DashboardSection::About),
    ];
    let right = area.x + area.width; // frame edge; ratatui does not clip for us
    let mut x = area.x;
    let mut underline: Vec<(u16, u16)> = Vec::new(); // (start_x, width) for active
    for (key, label, section) in tabs {
        if x >= right {
            break;
        }
        let active = app.section == section;
        let color = if active { theme::fg() } else { theme::fg_dim() };
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
            Paragraph::new(Line::from(Span::styled(divider, Style::default().fg(theme::border_dim())))),
            Rect { x: area.x, y: div_y, width: area.width, height: 1 },
        );
        for (sx, w) in underline {
            let w = w.min(right.saturating_sub(sx));
            if w == 0 {
                continue;
            }
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("━".repeat(w as usize), Style::default().fg(theme::blue())))),
                Rect { x: sx, y: div_y, width: w, height: 1 },
            );
        }
    }
}

/// Dispatch to the active dashboard section.
pub fn screen(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    match app.section {
        DashboardSection::Workspace => workspace(f, app, area, zones),
        DashboardSection::Settings => settings(f, app, area, zones),
        DashboardSection::About => about(f, area),
    }
}

/// The workspace section: a fixed two-column layout (recent files on the left,
/// sessions over stats on the right).
fn workspace(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    columns_layout(f, app, area, zones);
}

// --- columns layout --------------------------------------------------------

fn columns_layout(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .spacing(2)
        .split(area);

    // Left column: quick ACTIONS, then RECENT FILES sized to its (at most 8)
    // rows rather than stretching to fill the column — a tall terminal used
    // to leave it mostly blank space instead of handing that room to
    // anything else.
    let recent_h = app.project.recent_files.len().max(1) as u16 + 2;
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Length(recent_h), Constraint::Min(0)])
        .spacing(1)
        .split(cols[0]);
    actions_panel(f, left[0], zones);
    let inner = titled_panel(f, left[1], "RECENT FILES", theme::blue());
    recent_files_rows(f, app, inner, zones, true);

    // Right: SESSIONS, GIT STATUS, then STATS — also sized to its (fixed)
    // three rows instead of a `Min` that let it swallow the rest of the
    // column.
    let sessions = &app.project.sessions;
    let git_h = git_panel_height(app);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(sessions.len().max(1) as u16 * 2 + 3),
            Constraint::Length(git_h),
            Constraint::Length(5),
            Constraint::Min(0),
        ])
        .spacing(1)
        .split(cols[1]);

    sessions_rows(f, app, right[0], zones);

    // GIT STATUS panel ([g] expands it with remote / last commit / untracked).
    let g_inner = titled_panel(f, right[1], "[g] GIT STATUS", theme::purple());
    git_rows(f, app, g_inner);

    let st_inner = titled_panel(f, right[2], "STATS", theme::orange());
    let stats_data = &app.project.stats;
    let stat = |k: &'static str, v: String, c| {
        Line::from(vec![
            Span::styled(format!("{k:<9}"), Style::default().fg(theme::fg_dim())),
            Span::styled(v, Style::default().fg(c)),
        ])
    };
    // Plugin counts come straight from `config.plugins`, not the project
    // scan — see `crate::ui::plugins`.
    let plugins_loaded = app
        .config
        .plugins
        .iter()
        .filter(|p| app.plugin_status.get(&p.name) == Some(&crate::app::PluginLoadStatus::Loaded))
        .count();
    let stats = vec![
        stat("startup", format!("{}ms", stats_data.startup_ms), theme::green()),
        stat("plugins", format!("{}/{}", plugins_loaded, app.config.plugins.len()), theme::cyan()),
        // `…` while the background count is still running — a real number
        // needs every source file read, which does not block startup.
        stat(
            "loc",
            stats_data.loc.clone().unwrap_or_else(|| "…".to_string()),
            theme::purple(),
        ),
    ];
    f.render_widget(Paragraph::new(stats).style(Style::default().bg(theme::bg_dark())), st_inner);
}

/// The SESSIONS panel: one two-line row per recent project. The row for the
/// project currently open (`SessionEntry::last == "this session"`) carries a
/// right-aligned `[X]` badge — `App::discard_session` — that wipes its saved
/// tab list and unsaved-edit recovery snapshots, plus a `+` marker when there
/// are unsaved changes worth either keeping or clearing.
fn sessions_rows(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    let inner = titled_panel(f, area, "SESSIONS", theme::green());
    for (i, s) in app.project.sessions.iter().enumerate() {
        let y = inner.y + i as u16 * 2;
        if y >= inner.y + inner.height {
            break;
        }
        let is_current = s.last == "this session";

        let mut spans = vec![
            Span::styled(s.name.clone(), Style::default().fg(theme::fg())),
            Span::raw("  "),
            Span::styled(s.last.clone(), Style::default().fg(theme::fg_dim())),
        ];
        if is_current && app.has_unsaved() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("+", Style::default().fg(theme::orange())));
        }
        let row1 = Rect { x: inner.x, y, width: inner.width, height: 1 };
        if is_current {
            let badge = hint_badge('X');
            let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
            let bw = badge.width() as u16;
            if inner.width > used + bw {
                spans.push(Span::styled(" ".repeat((inner.width - used - bw) as usize), Style::default().bg(theme::bg_dark())));
            }
            let bx = inner.x + inner.width.saturating_sub(bw);
            spans.push(badge);
            zones.push(Rect { x: bx, y, width: bw.min(inner.width), height: 1 }, Action::DiscardSession);
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::bg_dark())), row1);

        if y + 1 < inner.y + inner.height {
            let row2 = Rect { x: inner.x, y: y + 1, width: inner.width, height: 1 };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("⌸ {} · {} files", s.branch, s.files),
                    Style::default().fg(theme::fg_dim()),
                )))
                .style(Style::default().bg(theme::bg_dark())),
                row2,
            );
        }
    }
}

/// The quick-ACTIONS pane: clickable **New File** and **Find Files** rows, each
/// with its keyboard shortcut — the dashboard home for these actions instead of
/// tucking them into the status-line hints.
fn actions_panel(f: &mut Frame, area: Rect, zones: &mut Zones) {
    let inner = titled_panel(f, area, "ACTIONS", theme::cyan());
    let rows: [(char, ratatui::style::Color, &str, char, Action); 6] = [
        ('N', theme::green(), "New File", 'n', Action::NewFile),
        ('F', theme::blue(), "Find Files", 'e', Action::OpenFinder),
        ('G', theme::orange(), "Find in Files", '/', Action::OpenGrepPrompt),
        ('P', theme::purple(), "Command Palette", ':', Action::OpenPalette),
        ('H', theme::cyan(), "Help", '?', Action::ToggleHelp),
        ('Q', theme::fg_dim(), "Quit", 'q', Action::RunEx("qa".to_string())),
    ];
    for (i, (letter, color, label, key, action)) in rows.into_iter().enumerate() {
        if i as u16 >= inner.height {
            break;
        }
        let y = inner.y + i as u16;
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        let mut spans = vec![
            Span::raw("  "),
            icon_chip(letter, color),
            Span::raw(" "),
            Span::styled(label, Style::default().fg(theme::fg())),
        ];
        // Right-aligned `[k]` shortcut badge.
        let badge = hint_badge(key);
        let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
        let bw = badge.width() as u16;
        if inner.width > used + bw {
            spans.push(Span::styled(" ".repeat((inner.width - used - bw) as usize), Style::default().bg(theme::bg_dark())));
        }
        spans.push(badge);
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::bg_dark())),
            row,
        );
        zones.push(row, action);
    }
}

/// Render the recent-files list (icon chip + name/path + modified), with the
/// keyboard-focused row highlighted. `full_path` picks path vs. name.
fn recent_files_rows(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones, full_path: bool) {
    if app.project.recent_files.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("no files", Style::default().fg(theme::fg_dim()))))
                .style(Style::default().bg(theme::bg_dark())),
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
            selection_bar(selected, theme::blue()),
            file_chip(&file.icon, app.config.icons),
            Span::raw(" "),
            Span::styled(format!("{label} "), Style::default().fg(theme::fg())),
        ];
        // Right-aligned modified timestamp.
        let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
        let mod_w = file.modified.chars().count() as u16;
        if area.width > used + mod_w {
            spans.push(Span::styled(" ".repeat((area.width - used - mod_w) as usize), row_style(selected)));
        }
        spans.push(Span::styled(file.modified.clone(), Style::default().fg(theme::fg_dim())));
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(row_style(selected)),
            row,
        );
        zones.push(row, Action::OpenFile(i));
    }
}

// --- git panel -------------------------------------------------------------

/// Total height the GIT STATUS panel wants, borders included.
///
/// Computed rather than fixed because the rows are conditional now — a clean
/// repo with no stashes is four lines shorter than a conflicted one mid-merge,
/// and a fixed height would either clip that or leave a hole in the common case.
fn git_panel_height(app: &App) -> u16 {
    const BORDERS: u16 = 2;
    const LEGEND: u16 = 2; // divider + key row
    let Some(g) = &app.project.git else {
        return 1 + BORDERS; // just the "not a git repository" line
    };
    let mut rows = 4; // branch, ahead/behind, modified, staged
    if g.conflicted > 0 {
        rows += 1;
    }
    if g.stashes > 0 {
        rows += 1;
    }
    if app.expand_git {
        rows += 4; // remote, untracked, last commit, subject
    }
    rows + LEGEND + BORDERS
}

fn git_rows(f: &mut Frame, app: &App, area: Rect) {
    let Some(g) = &app.project.git else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("not a git repository", Style::default().fg(theme::fg_dim()))))
                .style(Style::default().bg(theme::bg_dark())),
            area,
        );
        return;
    };
    let kv = |k: &'static str, val: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled(format!("{k:<16}"), Style::default().fg(theme::fg_dim()))];
        spans.extend(val);
        Line::from(spans)
    };
    // `modified` carries the diff size next to it: a count of files says how
    // scattered the work is, the line delta says how big it is, and the two
    // answer different questions.
    let mut modified_val = vec![Span::styled(g.modified.to_string(), Style::default().fg(theme::orange()))];
    if g.insertions > 0 || g.deletions > 0 {
        modified_val.push(Span::styled(
            format!("  +{}", g.insertions),
            Style::default().fg(theme::green()),
        ));
        modified_val.push(Span::styled(
            format!(" −{}", g.deletions),
            Style::default().fg(theme::red()),
        ));
    }
    let mut lines = vec![
        kv("branch", vec![
            Span::styled(format!(" {}", g.branch), Style::default().fg(theme::purple())),
            Span::styled(format!("  {}", g.oid), Style::default().fg(theme::fg_dim())),
        ]),
        kv("ahead / behind", vec![
            Span::styled(format!("↑{}", g.ahead), Style::default().fg(theme::green())),
            Span::raw(" "),
            Span::styled(format!("↓{}", g.behind), Style::default().fg(theme::red())),
        ]),
        kv("modified", modified_val),
        kv("staged", vec![Span::styled(g.staged.to_string(), Style::default().fg(theme::cyan()))]),
    ];
    // Stashes are easy to forget about, so they only earn a row when there is
    // something in there to forget.
    if g.stashes > 0 {
        lines.push(kv("stashes", vec![Span::styled(
            g.stashes.to_string(),
            Style::default().fg(theme::orange()),
        )]));
    }
    // Conflicts get a row only when there are some — an always-present "0"
    // would cost a line of a six-line panel to say nothing. When they exist
    // they matter more than anything else here, so the row goes first.
    if g.conflicted > 0 {
        lines.insert(
            0,
            kv("conflicts", vec![Span::styled(
                format!("{} unresolved", g.conflicted),
                Style::default().fg(theme::red()).add_modifier(Modifier::BOLD),
            )]),
        );
    }
    if app.expand_git {
        let remote = if g.remote.is_empty() { "—".to_string() } else { g.remote.clone() };
        lines.push(kv("remote", vec![Span::styled(remote, Style::default().fg(theme::fg_muted()))]));
        lines.push(kv("untracked", vec![Span::styled(g.untracked.to_string(), Style::default().fg(theme::red()))]));
        lines.push(kv("last commit", vec![
            Span::styled(g.last_commit.clone(), Style::default().fg(theme::fg_muted())),
            Span::styled(format!(" · {}", g.last_author), Style::default().fg(theme::fg_dim())),
        ]));
        // The subject is the one line here likely to overrun the panel, so it
        // gets its own row, indented under the timestamp it belongs to.
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(16)),
            Span::styled(
                truncate(&g.last_subject, area.width.saturating_sub(17) as usize),
                Style::default().fg(theme::fg_muted()),
            ),
        ]));
    }

    // The action legend, pinned to the bottom of the panel so the rows above
    // can grow (conflicts, stashes) without pushing it out of view.
    let legend_h = 2u16;
    let rows_h = area.height.saturating_sub(legend_h);
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme::bg_dark())),
        Rect { height: rows_h, ..area },
    );
    if area.height <= legend_h {
        return;
    }
    let y = area.y + rows_h;
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(theme::border_dim()),
        )))
        .style(Style::default().bg(theme::bg_dark())),
        Rect { y, height: 1, ..area },
    );
    let legend = Line::from(vec![
        hint_badge('c'),
        Span::styled(" files ", Style::default().fg(theme::fg_dim())),
        hint_badge('l'),
        Span::styled(" log ", Style::default().fg(theme::fg_dim())),
        hint_badge('d'),
        Span::styled(" diff ", Style::default().fg(theme::fg_dim())),
        hint_badge('F'),
        Span::styled(" fetch", Style::default().fg(theme::fg_dim())),
    ]);
    f.render_widget(
        Paragraph::new(legend).style(Style::default().bg(theme::bg_dark())),
        Rect { y: y + 1, height: 1, ..area },
    );
}

// --- settings (LSP) --------------------------------------------------------

/// A single labelled on/off setting row with a right-aligned toggle. Clicking
/// the row dispatches `action`; `selected` highlights the keyboard focus.
fn option_row(
    f: &mut Frame,
    row: Rect,
    label: &str,
    on: bool,
    selected: bool,
    zones: &mut Zones,
    action: Action,
) {
    f.render_widget(Block::default().style(row_style(selected)), row);
    let toggle = if on { "●on " } else { "○off" };
    let toggle_color = if on { theme::green() } else { theme::fg_dim() };
    let mut spans = vec![selection_bar(selected, theme::cyan()), Span::styled(label.to_string(), Style::default().fg(theme::fg()))];
    let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
    let tw = toggle.chars().count() as u16;
    if row.width > used + tw {
        spans.push(Span::styled(" ".repeat((row.width - used - tw) as usize), row_style(selected)));
    }
    spans.push(Span::styled(toggle, Style::default().fg(toggle_color)));
    f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
    zones.push(row, action);
}

/// Like [`option_row`], but for a setting with more than two states: the
/// right-aligned toggle is replaced by the current value.
fn choice_row(
    f: &mut Frame,
    row: Rect,
    label: &str,
    value: &str,
    selected: bool,
    zones: &mut Zones,
    action: Action,
) {
    f.render_widget(Block::default().style(row_style(selected)), row);
    let mut spans = vec![selection_bar(selected, theme::cyan()), Span::styled(label.to_string(), Style::default().fg(theme::fg()))];
    let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
    let vw = value.chars().count() as u16;
    if row.width > used + vw {
        spans.push(Span::styled(" ".repeat((row.width - used - vw) as usize), row_style(selected)));
    }
    spans.push(Span::styled(value.to_string(), Style::default().fg(theme::cyan())));
    f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
    zones.push(row, action);
}

/// Clip `s` to `max` characters, marking the cut with an ellipsis — keeps a
/// fixed-width column from swallowing the one after it when a registry entry
/// (e.g. a formatter's filetype list) runs long.
fn truncate(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max {
        return std::borrow::Cow::Borrowed(s);
    }
    std::borrow::Cow::Owned(format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()))
}

fn settings(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    if area.width < 10 || area.height < 4 {
        return;
    }
    // EDITOR options panel: live-toggleable settings backed by config.toml,
    // one row per `Config::to_setting_items()` entry. A `/` search narrows
    // `visible` to the matching real indices without renumbering them — see
    // `App::settings_matches`.
    let visible: Vec<usize> =
        if app.settings_search { app.settings_matches() } else { (0..App::SETTINGS_EDITOR_OPTIONS).collect() };
    let opt_h = 9u16.min(area.height);
    let opt_rect = Rect { height: opt_h, ..area };
    let cfg_hint = Line::from(Span::styled("┤ ~/.config/ctrlvim/config.toml ├", Style::default().fg(theme::fg_dim()).bg(theme::bg_dark())));
    let opt_inner = super::titled_panel_with_right(f, opt_rect, "EDITOR", theme::cyan(), Some(cfg_hint));
    let sel = app.settings_index;

    // `/` search field: dim hint when idle, live query + cursor when typing.
    if opt_inner.height >= 1 {
        let r = Rect { x: opt_inner.x, y: opt_inner.y, width: opt_inner.width, height: 1 };
        let line = if app.settings_search {
            Line::from(vec![
                Span::styled("/", Style::default().fg(theme::blue())),
                Span::styled(app.settings_query.clone(), Style::default().fg(theme::fg())),
                Span::styled("▏", Style::default().fg(theme::fg())),
            ])
        } else {
            Line::from(Span::styled("/ to search settings", Style::default().fg(theme::fg_dim())))
        };
        f.render_widget(Paragraph::new(line), r);
        if app.settings_search {
            let right = r.x + r.width.saturating_sub(1);
            f.set_cursor_position((
                (r.x + 1 + ctrlvim_core::display_width(&app.settings_query) as u16).min(right),
                r.y,
            ));
        }
    }

    let items = app.config.to_setting_items();
    for (row_i, &item_i) in visible.iter().enumerate() {
        let y = opt_inner.y + 1 + row_i as u16;
        if y >= opt_inner.y + opt_inner.height {
            break;
        }
        let r = Rect { x: opt_inner.x, y, width: opt_inner.width, height: 1 };
        let selected = sel == item_i;
        let Some(item) = items.get(item_i) else { continue };
        match item_i {
            4 => {
                // The *live* 'tabstop' (which tracks 'shiftwidth' too — see
                // `App::cycle_indent_width`), same reasoning as the AI row
                // below: `:set tabstop=8` from the command line shouldn't
                // leave this row showing a number the editor no longer
                // agrees with.
                choice_row(f, r, item.label, &app.tabstop().to_string(), selected, zones, Action::CycleIndentWidth);
            }
            5 => {
                // The *live* state, not `config.ai.enabled`: `:AI on` changes
                // one and not the other, and a checkbox that disagrees with
                // the editor is worse than no checkbox. Toggling here writes
                // the config as well.
                option_row(f, r, item.label, app.ai_enabled(), selected, zones, Action::ToggleAi);
            }
            _ => match &item.value {
                crate::config::SettingValue::Bool(on) => {
                    let action = match item.key {
                        "drawer" => Action::ToggleStartupDrawer,
                        "tabs" => Action::ToggleTabs,
                        "mouse" => Action::ToggleMouse,
                        _ => continue,
                    };
                    option_row(f, r, item.label, *on, selected, zones, action);
                }
                crate::config::SettingValue::Choice { current, .. } => {
                    choice_row(f, r, item.label, current, selected, zones, Action::CycleIconMode);
                }
                _ => {}
            },
        }
    }
    if visible.is_empty() {
        let r = Rect { x: opt_inner.x, y: opt_inner.y + 1, width: opt_inner.width, height: 1 };
        if r.y < opt_inner.y + opt_inner.height {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("no settings match", Style::default().fg(theme::fg_dim())))),
                r,
            );
        }
    }

    // Header row for the LSP table.
    let hdr_y = area.y + opt_h + 1;
    if hdr_y < area.y + area.height {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("LANGUAGE SERVERS & LINKERS", Style::default().fg(theme::fg_dim())))),
            Rect { y: hdr_y, height: 1, ..area },
        );
        let active = format!("{} active", app.lsp_active_count());
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(active, Style::default().fg(theme::fg_dim()))).right_aligned()),
            Rect { y: hdr_y, height: 1, ..area },
        );
    }

    // Size the table to its content (header + rows + borders) so the footer
    // sits cleanly beneath it rather than on the bottom border.
    let table_y = hdr_y + 2;
    let avail = (area.y + area.height).saturating_sub(table_y);
    let table_h = (app.lsp.len() as u16 + 3).min(avail);
    let table = Rect { y: table_y, height: table_h, ..area };
    let inner = super::titled_panel(f, table, "LSP", theme::orange());

    if app.lsp.is_empty() {
        if inner.height > 0 {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "No servers declared. Add an entry to lsp.lua to see it here.",
                    Style::default().fg(theme::fg_dim()),
                )))
                .style(Style::default().bg(theme::bg_dark())),
                Rect { height: 1, ..inner },
            );
        }
    } else {
        // Column header.
        let ch = Line::from(vec![
            Span::styled(format!("{:<18}", "SERVER"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<20}", "FILETYPES"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<12}", "STATUS"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
        ]);
        if inner.height > 0 {
            f.render_widget(Paragraph::new(ch).style(Style::default().bg(theme::bg_dark())), Rect { height: 1, ..inner });
        }

        for (i, lsp) in app.lsp.iter().enumerate() {
            let y = inner.y + 1 + i as u16;
            if y >= inner.y + inner.height {
                break;
            }
            let on = app.lsp_enabled[i];
            // The Settings selection spans the EDITOR options first, then the LSPs.
            let selected = app.settings_index == i + App::SETTINGS_EDITOR_OPTIONS;
            let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
            f.render_widget(Block::default().style(row_style(selected)), row);

            // Status reflects a live `PATH` check + the on/off toggle. A
            // missing server with a declared `install` command is orange
            // (actionable via `I`); one with none is red.
            let (status_label, status_color) = if !lsp.installed {
                ("not found", if lsp.install.is_some() { theme::orange() } else { theme::red() })
            } else if on {
                ("running", theme::green())
            } else {
                ("disabled", theme::fg_dim())
            };
            let toggle = if on { "●on " } else { "○off" };
            let toggle_color = if on { theme::green() } else { theme::fg_dim() };
            let filetypes = if lsp.filetypes.is_empty() { "—" } else { &lsp.filetypes };
            let spans = vec![
                selection_bar(selected, theme::blue()),
                // Truncate one character short of the column width so a clipped
                // entry still leaves at least one space before the next column.
                Span::styled(format!("{:<18}", truncate(&lsp.name, 17)), Style::default().fg(theme::fg())),
                Span::styled(format!("{:<20}", truncate(filetypes, 19)), Style::default().fg(theme::fg_dim())),
                Span::styled(format!("{:<12}", status_label), Style::default().fg(status_color)),
                Span::styled(toggle, Style::default().fg(toggle_color)),
            ];
            f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
            zones.push(row, Action::SetSettingsIndex(i + App::SETTINGS_EDITOR_OPTIONS));
            // The toggle glyph itself flips the server.
            let toggle_w = 4u16;
            let tx = row.x + row.width.saturating_sub(toggle_w);
            zones.push(Rect { x: tx, y, width: toggle_w, height: 1 }, Action::ToggleLsp(i));
        }
    }

    // Footer: a per-row install hint (the declared command itself) when the
    // focused row has one, otherwise the config file this whole table is a
    // view of.
    let foot_y = table.y + table_h + 1;
    if foot_y < area.y + area.height {
        let focused = app.settings_index.checked_sub(App::SETTINGS_EDITOR_OPTIONS).and_then(|i| app.lsp.get(i));
        let footer = match focused {
            Some(lsp) if !lsp.installed && lsp.install.is_some() => Line::from(vec![Span::styled(
                format!("Press I to install {}: {}", lsp.name, truncate(lsp.install.as_deref().unwrap_or(""), 70)),
                Style::default().fg(theme::orange()),
            )]),
            _ => {
                let path = crate::lsp_config::path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "~/.config/ctrlvim/lsp.lua".to_string());
                Line::from(vec![
                    Span::styled("Config file: ", Style::default().fg(theme::fg_dim())),
                    Span::styled(path, Style::default().fg(theme::cyan())),
                ])
            }
        };
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
        centered_at!(y, Line::from(Span::styled(l, Style::default().fg(theme::blue()))));
        y += 1;
    }
    y += 1;
    centered_at!(y, Line::from(Span::styled("ctrlvim", Style::default().fg(theme::fg()).add_modifier(Modifier::BOLD))));
    y += 1;
    centered_at!(y, Line::from(Span::styled("a rust tui editor · v0.4.2", Style::default().fg(theme::fg_dim()))));
    y += 2;

    let kv = |k: &'static str, v: &'static str, vc| {
        Line::from(vec![
            Span::styled(format!("{k:<10}"), Style::default().fg(theme::fg_dim())),
            Span::styled(v, Style::default().fg(vc)),
        ])
    };
    let rows = [
        kv("rust", "1.82.0", theme::fg()),
        kv("ratatui", "0.29", theme::fg()),
        kv("crossterm", "0.28", theme::fg()),
        kv("license", "MIT", theme::fg()),
        kv("repo", "github.com/CtrlUserKnown/ctrlvim", theme::cyan()),
    ];
    for line in rows {
        if y >= area.y + area.height {
            break;
        }
        f.render_widget(Paragraph::new(line), Rect { x: cx, y, width: block_w, height: 1 });
        y += 1;
    }
}
