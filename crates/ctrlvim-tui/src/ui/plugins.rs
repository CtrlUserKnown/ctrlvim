//! The Plugin Manager buffer: a full-width table of every `[[plugin]]` entry
//! declared in `config.toml`.
//!
//! This reads `app.config.plugins` (plus `app.plugin_status`, which records
//! what actually happened the one time each entry was loaded) and nothing
//! else — no filesystem scan of any plugin directory. A plugin absent from
//! `config.toml` does not exist here, not even as an "off" row: absence from
//! config means absence from the editor.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, PluginLoadStatus};
use crate::theme;

pub fn screen(f: &mut Frame, app: &App, area: Rect) {
    if area.width < 10 || area.height < 4 {
        return;
    }
    let plugins = &app.config.plugins;
    let loaded =
        plugins.iter().filter(|p| app.plugin_status.get(&p.name) == Some(&PluginLoadStatus::Loaded)).count();

    // Header: "Plugin Manager"    "N loaded · M configured".
    let title = Line::from(Span::styled(
        "Plugin Manager",
        Style::default().fg(theme::fg()).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(title), Rect { height: 1, ..area });
    let summary = format!("{loaded} loaded · {} configured", plugins.len());
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(summary, Style::default().fg(theme::fg_dim()))).right_aligned()),
        Rect { height: 1, ..area },
    );

    if plugins.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No plugins configured. Add a [[plugin]] entry to config.toml to see it here.",
                Style::default().fg(theme::fg_dim()),
            )))
            .style(Style::default().bg(theme::bg())),
            Rect { y: area.y + 2, height: 1, ..area },
        );
        return;
    }

    let table_h = area.height.saturating_sub(4); // header rows + footer
    let table = Rect { y: area.y + 2, height: table_h, ..area };
    let inner = super::titled_panel(f, table, "PLUGINS", theme::orange());
    if inner.height == 0 {
        return;
    }

    let name_w = 18usize;
    let path_w = 40usize;
    let header = Line::from(vec![
        Span::styled(format!("{:<name_w$}", "NAME"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<path_w$}", "PATH"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
        Span::styled("STATUS", Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(header).style(Style::default().bg(theme::bg_dark())), Rect { height: 1, ..inner });

    // Loosely join plugin-registered commands (tagged with their source
    // script's file stem) against each entry's `name`, so a plugin that
    // actually ran shows what it contributed. A plugin whose file stem
    // doesn't match its config `name` just shows no commands.
    let plugin_cmds = app.engine.plugin_commands();
    let commands_for = |name: &str| -> String {
        let mut names: Vec<&str> = plugin_cmds
            .iter()
            .filter(|(_, _, source)| source.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(name)))
            .map(|(cmd, _, _)| cmd.as_str())
            .collect();
        names.sort_unstable();
        names.join(", ")
    };

    let mut lines: Vec<Line> = Vec::new();
    for p in plugins {
        let (status_label, status_color) = plugin_status_label(app, p);
        let mut spans = vec![
            Span::styled(format!("{:<name_w$}", truncate(&p.name, name_w - 1)), Style::default().fg(theme::fg())),
            Span::styled(format!("{:<path_w$}", truncate(&p.path, path_w - 1)), Style::default().fg(theme::fg_dim())),
            Span::styled(status_label, Style::default().fg(status_color)),
        ];
        let commands = commands_for(&p.name);
        if !commands.is_empty() {
            spans.push(Span::styled(format!("  commands: {commands}"), Style::default().fg(theme::fg_dim())));
        }
        lines.push(Line::from(spans));
    }
    let rows = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::bg_dark())), rows);

    // Footer: the config file this whole screen is a view of.
    let foot_y = table.y + table_h;
    if foot_y < area.y + area.height {
        let path = crate::config::Config::path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.config/ctrlvim/config.toml".to_string());
        let footer = Line::from(vec![
            Span::styled("Config file: ", Style::default().fg(theme::fg_dim())),
            Span::styled(path, Style::default().fg(theme::cyan())),
        ]);
        f.render_widget(Paragraph::new(footer), Rect { x: area.x, y: foot_y, width: area.width, height: 1 });
    }
}

/// Status text + color for one `[[plugin]]` entry, derived entirely from
/// `config.toml` state (`enabled`, `event`) and the one-shot load outcome in
/// `app.plugin_status` — never from the filesystem.
fn plugin_status_label(app: &App, p: &crate::config::PluginEntry) -> (String, Color) {
    match app.plugin_status.get(&p.name) {
        Some(PluginLoadStatus::Loaded) => ("loaded".to_string(), theme::green()),
        Some(PluginLoadStatus::Error(e)) => (format!("error: {}", first_line(e)), theme::red()),
        None if !p.enabled => ("off".to_string(), theme::fg_dim()),
        None => match &p.event {
            Some(event) => (format!("waiting: {event}"), theme::fg_dim()),
            // Enabled, immediate, but never reached the loader — shouldn't
            // normally happen outside of tests that build a bare `App`.
            None => ("pending".to_string(), theme::fg_dim()),
        },
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// Truncate to `max` characters, without splitting a multi-byte char.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}
