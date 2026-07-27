//! The Plugin Manager buffer: a full-width table of all plugins.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::model::PluginStatus;
use crate::theme;

pub fn screen(f: &mut Frame, app: &App, area: Rect) {
    if area.width < 10 || area.height < 4 {
        return;
    }
    let plugins = &app.project.plugins;
    let updates = plugins.iter().filter(|p| p.status == PluginStatus::Update).count();

    // Header: "Plugin Manager"    "N loaded · M updates available".
    let title = Line::from(Span::styled(
        "Plugin Manager",
        Style::default().fg(theme::fg()).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(title), Rect { height: 1, ..area });
    let summary = format!(
        "{} loaded · {} updates available",
        app.project.stats.plugins_loaded, updates
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(summary, Style::default().fg(theme::fg_dim()))).right_aligned()),
        Rect { height: 1, ..area },
    );

    if plugins.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No plugins installed. Drop plugins under ~/.config/ctrlvim/pack/*/start/ to see them here.",
                Style::default().fg(theme::fg_dim()),
            )))
            .style(Style::default().bg(theme::bg())),
            Rect { y: area.y + 2, height: 1, ..area },
        );
        return;
    }

    let table = Rect { y: area.y + 2, height: area.height.saturating_sub(2), ..area };
    let inner = super::titled_panel(f, table, "PLUGINS", theme::orange());
    if inner.height == 0 {
        return;
    }

    // Column widths roughly 2 : 3 : 1.4 : 1.
    let name_w = 18usize;
    let repo_w = 26usize;
    let cat_w = 14usize;
    let header = Line::from(vec![
        Span::styled(format!("{:<name_w$}", "NAME"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<repo_w$}", "REPO"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<cat_w$}", "CATEGORY"), Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
        Span::styled("STATUS", Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD)),
    ]);
    f.render_widget(Paragraph::new(header).style(Style::default().bg(theme::bg_dark())), Rect { height: 1, ..inner });

    // Loosely join plugin-registered commands (tagged with their source
    // script's file stem) against the scanned pack-directory names, so an
    // installed plugin that actually ran shows what it contributed. `config
    // .plugins` scripts and the pack directory are otherwise unrelated
    // systems, so a plugin with no matching source just shows no commands.
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
        let repo = if p.repo.is_empty() { "—" } else { p.repo.as_str() };
        let cat = if p.category.is_empty() { "—" } else { p.category.as_str() };
        let mut spans = vec![
            Span::styled(format!("{:<name_w$}", p.name), Style::default().fg(theme::fg())),
            Span::styled(format!("{repo:<repo_w$}"), Style::default().fg(theme::fg_dim())),
            Span::styled(format!("{cat:<cat_w$}"), Style::default().fg(theme::cyan())),
            Span::styled(p.status.label(), Style::default().fg(p.status.color())),
        ];
        let commands = commands_for(&p.name);
        if !commands.is_empty() {
            spans.push(Span::styled(format!("  commands: {commands}"), Style::default().fg(theme::fg_dim())));
        }
        lines.push(Line::from(spans));
    }
    let rows = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme::bg_dark())), rows);
}
