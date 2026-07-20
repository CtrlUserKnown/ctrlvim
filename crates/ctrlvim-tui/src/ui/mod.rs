//! Rendering. Mirrors the design's visual structure: a shell (tab bar +
//! status line) wrapping a body that switches on the active buffer, with the
//! explorer / palette / help drawn last as floating overlays.
//!
//! Mouse support is provided by a click-zone registry: each frame, interactive
//! elements register a [`ClickZone`] (a `Rect` + an [`Action`]); a mouse click
//! looks up the top-most matching zone and dispatches the same action its
//! keyboard equivalent would.

mod dashboard;
mod filebuffer;
mod overlays;
mod plugins;
mod shell;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

use crate::app::{Action, App, BufferKind, DashboardSection};
use crate::theme;

/// A rectangular hit target and the action a click on it performs.
pub struct ClickZone {
    pub area: Rect,
    pub action: Action,
}

/// Accumulates click zones for the current frame; later `push`es sit "on top"
/// so overlays win over the content beneath them.
#[derive(Default)]
pub struct Zones(pub Vec<ClickZone>);

impl Zones {
    pub fn push(&mut self, area: Rect, action: Action) {
        self.0.push(ClickZone { area, action });
    }
    /// Top-most zone containing the point, or `None`.
    pub fn hit(&self, col: u16, row: u16) -> Option<&Action> {
        self.0
            .iter()
            .rev()
            .find(|z| contains(z.area, col, row))
            .map(|z| &z.action)
    }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Draw the whole UI and return the frame's click zones.
pub fn draw(f: &mut Frame, app: &App) -> Zones {
    let mut zones = Zones::default();
    let area = f.area();

    // Fill the background.
    f.render_widget(Block::default().style(Style::default().bg(theme::BG)), area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // body
            Constraint::Length(1), // status line
        ])
        .split(area);

    shell::tab_bar(f, app, rows[0], &mut zones);
    body(f, app, rows[1], &mut zones);
    shell::status_line(f, app, rows[2]);

    // Overlays, drawn last (and registered last, so they win hit-testing).
    if app.sidebar_visible {
        overlays::explorer(f, app, area, &mut zones);
    }
    if app.help_open {
        overlays::help(f, app, area, &mut zones);
    }
    if app.palette_open {
        overlays::palette(f, app, area, &mut zones);
    }

    zones
}

/// The padded content region between the tab bar and status line.
fn body(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    // Padding to echo the prototype's generous 32px content padding.
    let inner = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(1),
    };
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The dashboard header (logo + section tabs) shows only on the Dashboard
    // buffer; the keybindings sidebar shows whenever the section is Workspace,
    // independent of which buffer is active (faithful to the final design).
    let mut region = inner;
    if matches!(app.active_buffer().kind, BufferKind::Dashboard) {
        let header_h = 4;
        let header = Rect { height: header_h.min(region.height), ..region };
        dashboard::header(f, app, header, zones);
        region = Rect {
            y: region.y + header.height,
            height: region.height.saturating_sub(header.height),
            ..region
        };
    }

    // Split off the keybindings sidebar when in the Workspace section.
    let content = if app.section == DashboardSection::Workspace {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(24), Constraint::Min(0)])
            .spacing(1)
            .split(region);
        dashboard::keybindings_sidebar(f, cols[0]);
        cols[1]
    } else {
        region
    };

    if content.width == 0 || content.height == 0 {
        return;
    }
    match &app.active_buffer().kind {
        BufferKind::Dashboard => dashboard::screen(f, app, content, zones),
        BufferKind::Plugins => plugins::screen(f, app, content),
        BufferKind::File(idx) => filebuffer::screen(f, app, *idx, content),
    }
}

// --- shared drawing helpers ------------------------------------------------

/// A bordered panel whose title floats on the top border, e.g.
/// `┤ RECENT FILES ├`, in the given accent color. Returns the inner area.
pub fn titled_panel(f: &mut Frame, area: Rect, name: &str, accent: Color) -> Rect {
    titled_panel_with_right(f, area, name, accent, None)
}

/// Like [`titled_panel`] but adds a right-aligned segment on the top border
/// (used for the grid panels' expand toggle glyph).
pub fn titled_panel_with_right(
    f: &mut Frame,
    area: Rect,
    name: &str,
    accent: Color,
    right: Option<Line<'static>>,
) -> Rect {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG_DARK))
        .title(Line::from(vec![Span::styled(
            format!("┤ {name} ├"),
            Style::default().fg(accent).bg(theme::BG_DARK),
        )]));
    if let Some(r) = right {
        block = block.title(r.right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// The colored two-letter file icon chip (` R `), bg = file color.
pub fn icon_chip(letter: char, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {letter} "),
        Style::default().fg(theme::BG_DARK).bg(color),
    )
}

/// Prefix span for a selected row: a `▎` accent bar (or two spaces when not
/// selected) so rows stay vertically aligned.
pub fn selection_bar(selected: bool, accent: Color) -> Span<'static> {
    if selected {
        Span::styled("▎ ", Style::default().fg(accent))
    } else {
        Span::raw("  ")
    }
}

/// Background style for a row, highlighted when selected.
pub fn row_style(selected: bool) -> Style {
    if selected {
        Style::default().bg(theme::BG_HIGHLIGHT)
    } else {
        Style::default().bg(theme::BG_DARK)
    }
}

/// A small `[x]`/chip keybinding hint badge (bracket variant — the default).
pub fn hint_badge(key: char) -> Span<'static> {
    Span::styled(format!("[{key}]"), Style::default().fg(theme::FG_DIM))
}

/// Center a `w`×`h` rect within `area`.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}
