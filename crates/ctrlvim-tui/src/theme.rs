//! Tokyo Night–inspired palette, matching the design's exact hex values.
//!
//! Names mirror the handoff spec so the UI code reads like the reference.

use ctrlvim_markdown::MdKind;
use ratatui::style::{Color, Modifier, Style};

/// background, editor area — `#1a1b26`
pub const BG: Color = Color::Rgb(0x1a, 0x1b, 0x26);
/// tab bar / status line / panel fill — `#16161e`
pub const BG_DARK: Color = Color::Rgb(0x16, 0x16, 0x1e);
/// selected row — `#1f2335`
pub const BG_HIGHLIGHT: Color = Color::Rgb(0x1f, 0x23, 0x35);
/// default panel border — `#3b4261`
pub const BORDER: Color = Color::Rgb(0x3b, 0x42, 0x61);
/// hairline dividers — `#292e42`
pub const BORDER_DIM: Color = Color::Rgb(0x29, 0x2e, 0x42);
/// primary text — `#c0caf5`
pub const FG: Color = Color::Rgb(0xc0, 0xca, 0xf5);
/// secondary / muted text, hints — `#565f89`
pub const FG_DIM: Color = Color::Rgb(0x56, 0x5f, 0x89);
/// slightly brighter muted text used for a few values — `#a9b1d6`
pub const FG_MUTED: Color = Color::Rgb(0xa9, 0xb1, 0xd6);
/// file/dir icons, workspace accents — `#7aa2f7`
pub const BLUE: Color = Color::Rgb(0x7a, 0xa2, 0xf7);
/// types, filetypes, links — `#7dcfff`
pub const CYAN: Color = Color::Rgb(0x7d, 0xcf, 0xff);
/// success/loaded/running/staged — `#9ece6a`
pub const GREEN: Color = Color::Rgb(0x9e, 0xce, 0x6a);
/// keywords, git branch, active dot — `#bb9af7`
pub const PURPLE: Color = Color::Rgb(0xbb, 0x9a, 0xf7);
/// warnings, plugin/settings accents, `.rs`/`.toml` icons — `#e0af68`
pub const ORANGE: Color = Color::Rgb(0xe0, 0xaf, 0x68);
/// errors, "update" status — `#f7768e`
pub const RED: Color = Color::Rgb(0xf7, 0x76, 0x8e);

/// A faint fill behind code spans/blocks, à la glamour's code background.
pub const CODE_BG: Color = Color::Rgb(0x24, 0x28, 0x3b);

/// Map a markdown [`MdKind`] to a concrete style, the way `glamour`'s dark
/// theme dresses a rendered document — but in ctrlvim's Tokyo Night palette so
/// the live preview stays cohesive with the rest of the editor.
///
/// Headings descend through the accent ramp (H1 brightest) to give the same
/// scannable hierarchy glow renders. Concealed markers ([`MdKind::Marker`])
/// are dimmed rather than hidden on the cursor's line, so editing stays legible.
pub fn md_style(kind: MdKind) -> Style {
    let s = Style::default();
    match kind {
        MdKind::Text => s.fg(FG),
        MdKind::Heading(1) => s.fg(PURPLE).add_modifier(Modifier::BOLD),
        MdKind::Heading(2) => s.fg(BLUE).add_modifier(Modifier::BOLD),
        MdKind::Heading(3) => s.fg(CYAN).add_modifier(Modifier::BOLD),
        MdKind::Heading(4) => s.fg(GREEN).add_modifier(Modifier::BOLD),
        MdKind::Heading(5) => s.fg(ORANGE).add_modifier(Modifier::BOLD),
        MdKind::Heading(_) => s.fg(FG_MUTED).add_modifier(Modifier::BOLD),
        MdKind::Bold => s.fg(FG).add_modifier(Modifier::BOLD),
        MdKind::Italic => s.fg(FG).add_modifier(Modifier::ITALIC),
        MdKind::BoldItalic => s.fg(FG).add_modifier(Modifier::BOLD | Modifier::ITALIC),
        MdKind::Code => s.fg(GREEN).bg(CODE_BG),
        MdKind::CodeBlock => s.fg(FG_MUTED).bg(CODE_BG),
        MdKind::CodeFence => s.fg(FG_DIM).bg(CODE_BG),
        MdKind::Quote => s.fg(FG_DIM).add_modifier(Modifier::ITALIC),
        MdKind::QuoteMarker => s.fg(PURPLE),
        MdKind::ListMarker => s.fg(BLUE),
        MdKind::Checkbox(true) => s.fg(GREEN),
        MdKind::Checkbox(false) => s.fg(FG_DIM),
        MdKind::Link => s.fg(CYAN).add_modifier(Modifier::UNDERLINED),
        MdKind::Rule => s.fg(BORDER),
        MdKind::Marker => s.fg(FG_DIM),
    }
}
