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
mod finder;
mod overlays;
mod plugins;
mod quickfix;
mod replace;
mod shell;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::Frame;

use crate::app::{Action, App, BufferKind};
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
        self.hit_zone(col, row).map(|z| &z.action)
    }

    /// Like [`hit`](Self::hit), but returns the whole zone — its `area` too,
    /// for an action (like [`Action::EditorClick`]) whose handler needs to
    /// know where inside the zone the click landed.
    pub fn hit_zone(&self, col: u16, row: u16) -> Option<&ClickZone> {
        self.0.iter().rev().find(|z| contains(z.area, col, row))
    }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// Whether the editor's own cursor is the one the terminal should show.
///
/// Exactly one surface may place the real cursor per frame, and overlays draw
/// after the body — so an overlay that takes typing just sets it again and
/// wins. The editor has to opt *out* instead, because a modal that takes no
/// text (help, the save prompt, `:!` output) would otherwise leave a cursor
/// blinking on the file behind it. When nobody claims it, ratatui hides it,
/// which is the right answer for those modals.
fn editor_owns_cursor(app: &App) -> bool {
    !app.palette_open
        && !app.help_open
        && !app.shell_open
        && !app.drawer_search
        && app.save_prompt.is_none()
        && app.finder.is_none()
        && app.replace.is_none()
}

/// Draw the whole UI and return the frame's click zones.
pub fn draw(f: &mut Frame, app: &App) -> Zones {
    let mut zones = Zones::default();
    let area = f.area();

    // Fill the background.
    f.render_widget(Block::default().style(Style::default().bg(theme::bg())), area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // body
            Constraint::Length(1), // status line
        ])
        .split(area);

    shell::tab_bar(f, app, rows[0], &mut zones);
    body(f, app, rows[1], &mut zones, editor_owns_cursor(app));
    shell::status_line(f, app, rows[2]);

    // Overlays, drawn last (and registered last, so they win hit-testing).
    if app.sidebar_visible {
        overlays::explorer(f, app, area, &mut zones);
    }
    if app.finder.is_some() {
        finder::screen(f, app, area, &mut zones);
    }
    if app.replace.is_some() {
        replace::screen(f, app, area, &mut zones);
    }
    if app.help_open {
        overlays::help(f, app, area, &mut zones);
    }
    if app.palette_open {
        overlays::palette(f, app, area, &mut zones);
    }
    if app.save_prompt.is_some() {
        overlays::save_prompt(f, app, area, &mut zones);
    }
    if app.shell_open {
        overlays::shell_output(f, app, area, &mut zones);
    }
    // Last of all: the which-key popup is transient and must sit on top of
    // whatever is already showing. It registers no zones — it's a hint, not a
    // menu, and clicking through it should hit what's underneath.
    overlays::which_key(f, app, area);

    zones
}

/// The padded content region between the tab bar and status line.
fn body(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones, owns_cursor: bool) {
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
    // buffer.
    let mut content = inner;
    if matches!(app.active_buffer().kind, BufferKind::Dashboard) {
        let header_h = 4;
        let header = Rect { height: header_h.min(content.height), ..content };
        dashboard::header(f, app, header, zones);
        content = Rect {
            y: content.y + header.height,
            height: content.height.saturating_sub(header.height),
            ..content
        };
    }

    // The quickfix pane takes a strip off the bottom, leaving the rest to the
    // buffer — the same way Vim's quickfix window splits the frame.
    let (content, qf_pane) = quickfix::split(app, content);

    if content.width == 0 || content.height == 0 {
        return;
    }
    match &app.active_buffer().kind {
        BufferKind::Dashboard => dashboard::screen(f, app, content, zones),
        BufferKind::Plugins => plugins::screen(f, app, content),
        BufferKind::File => filebuffer::screen(f, app, content, zones, owns_cursor),
    }
    if let Some(pane) = qf_pane {
        quickfix::screen(f, app, pane, zones);
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
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg_dark()))
        .title(Line::from(vec![Span::styled(
            format!("┤ {name} ├"),
            Style::default().fg(accent).bg(theme::bg_dark()),
        )]));
    if let Some(r) = right {
        block = block.title(r.right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// The colored two-letter file icon chip (` R `), bg = file color.
///
/// The letter is drawn in [`theme::on_accent`], which documents why a chip
/// can't use `theme::bg_dark()` for this.
pub fn icon_chip(letter: char, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {letter} "),
        Style::default().fg(theme::on_accent()).bg(color),
    )
}

/// The chip for a file entry: a Nerd Font glyph, or its letter fallback.
pub fn file_chip(icon: &crate::model::FileIcon, mode: crate::icons::IconMode) -> Span<'static> {
    icon_chip(icon.label(mode), icon.color)
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
        Style::default().bg(theme::bg_highlight())
    } else {
        Style::default().bg(theme::bg_dark())
    }
}

/// A small `[x]`/chip keybinding hint badge (bracket variant — the default).
pub fn hint_badge(key: char) -> Span<'static> {
    Span::styled(format!("[{key}]"), Style::default().fg(theme::fg_dim()))
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
