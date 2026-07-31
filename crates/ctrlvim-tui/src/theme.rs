//! Runtime, switchable color themes.
//!
//! The palette used to be a set of `pub const` colors; it is now a [`Theme`]
//! struct chosen at runtime (so `:Theme …` / the command palette can swap it)
//! and read through the `theme::bg()`-style accessors below. The active theme
//! lives behind a process-global lock; every render reads it.
//!
//! The theme roster mirrors the charvim repo's theme-switcher: a **Terminal**
//! default that follows the host terminal's own palette (ANSI colors +
//! `Color::Reset`), then Tokyo Night, Catppuccin, Noir-cat, Rosé Pine, Gruvbox,
//! and Habamax.

use std::sync::RwLock;

use ctrlvim_core::HlKind;
use ctrlvim_markdown::MdKind;
use ratatui::style::{Color, Modifier, Style};

/// A complete color palette. All fields are `Copy` colors so a `Theme` can sit
/// behind a lock and be swapped wholesale at runtime.
#[derive(Clone, Copy)]
pub struct Theme {
    /// Display name, as shown in the command palette / persisted to disk.
    pub name: &'static str,
    /// background, editor area
    pub bg: Color,
    /// tab bar / status line / panel fill
    pub bg_dark: Color,
    /// selected row
    pub bg_highlight: Color,
    /// default panel border
    pub border: Color,
    /// hairline dividers
    pub border_dim: Color,
    /// primary text
    pub fg: Color,
    /// secondary / muted text, hints
    pub fg_dim: Color,
    /// slightly brighter muted text used for a few values
    pub fg_muted: Color,
    /// file/dir icons, workspace accents
    pub blue: Color,
    /// types, filetypes, links
    pub cyan: Color,
    /// success/loaded/running/staged
    pub green: Color,
    /// keywords, git branch, active dot
    pub purple: Color,
    /// warnings, plugin/settings accents
    pub orange: Color,
    /// errors, "update" status
    pub red: Color,
    /// faint fill behind code spans/blocks
    pub code_bg: Color,
    /// background behind the active visual-mode selection
    pub selection: Color,
    /// background behind `hlsearch` search matches
    pub search: Color,
}

/// **Terminal (Auto)** — follows the host terminal's own theme. Backgrounds and
/// primary text use [`Color::Reset`] (the terminal's defaults); accents map to
/// the 16 ANSI colors so they track whatever palette the terminal is set to.
pub const TERMINAL: Theme = Theme {
    name: "Terminal (Auto)",
    bg: Color::Reset,
    bg_dark: Color::Reset,
    bg_highlight: Color::Indexed(8),
    border: Color::Indexed(8),
    border_dim: Color::Indexed(8),
    fg: Color::Reset,
    fg_dim: Color::DarkGray,
    fg_muted: Color::Gray,
    blue: Color::Blue,
    cyan: Color::Cyan,
    green: Color::Green,
    purple: Color::Magenta,
    orange: Color::Yellow,
    red: Color::Red,
    code_bg: Color::Indexed(8),
    selection: Color::Indexed(8),
    search: Color::Yellow,
};

/// **Tokyo Night** — the original design-handoff palette (`night` variant).
pub const TOKYO_NIGHT: Theme = Theme {
    name: "Tokyo Night",
    bg: Color::Rgb(0x1a, 0x1b, 0x26),
    bg_dark: Color::Rgb(0x16, 0x16, 0x1e),
    bg_highlight: Color::Rgb(0x1f, 0x23, 0x35),
    border: Color::Rgb(0x3b, 0x42, 0x61),
    border_dim: Color::Rgb(0x29, 0x2e, 0x42),
    fg: Color::Rgb(0xc0, 0xca, 0xf5),
    fg_dim: Color::Rgb(0x56, 0x5f, 0x89),
    fg_muted: Color::Rgb(0xa9, 0xb1, 0xd6),
    blue: Color::Rgb(0x7a, 0xa2, 0xf7),
    cyan: Color::Rgb(0x7d, 0xcf, 0xff),
    green: Color::Rgb(0x9e, 0xce, 0x6a),
    purple: Color::Rgb(0xbb, 0x9a, 0xf7),
    orange: Color::Rgb(0xe0, 0xaf, 0x68),
    red: Color::Rgb(0xf7, 0x76, 0x8e),
    code_bg: Color::Rgb(0x24, 0x28, 0x3b),
    selection: Color::Rgb(0x2d, 0x3f, 0x76),
    search: Color::Rgb(0xe0, 0xaf, 0x68),
};

/// **Catppuccin** — the `mocha` flavour.
pub const CATPPUCCIN: Theme = Theme {
    name: "Catppuccin",
    bg: Color::Rgb(0x1e, 0x1e, 0x2e),
    bg_dark: Color::Rgb(0x18, 0x18, 0x25),
    bg_highlight: Color::Rgb(0x31, 0x32, 0x44),
    border: Color::Rgb(0x45, 0x47, 0x5a),
    border_dim: Color::Rgb(0x31, 0x32, 0x44),
    fg: Color::Rgb(0xcd, 0xd6, 0xf4),
    fg_dim: Color::Rgb(0x6c, 0x70, 0x86),
    fg_muted: Color::Rgb(0xa6, 0xad, 0xc8),
    blue: Color::Rgb(0x89, 0xb4, 0xfa),
    cyan: Color::Rgb(0x89, 0xdc, 0xeb),
    green: Color::Rgb(0xa6, 0xe3, 0xa1),
    purple: Color::Rgb(0xcb, 0xa6, 0xf7),
    orange: Color::Rgb(0xfa, 0xb3, 0x87),
    red: Color::Rgb(0xf3, 0x8b, 0xa8),
    code_bg: Color::Rgb(0x31, 0x32, 0x44),
    selection: Color::Rgb(0x45, 0x47, 0x5a),
    search: Color::Rgb(0xf9, 0xe2, 0xaf),
};

/// **Noir-cat** — charvim's custom near-black theme with Catppuccin accents.
pub const NOIR_CAT: Theme = Theme {
    name: "Noir-cat",
    bg: Color::Rgb(0x1a, 0x1a, 0x1a),
    bg_dark: Color::Rgb(0x14, 0x14, 0x14),
    bg_highlight: Color::Rgb(0x31, 0x32, 0x44),
    border: Color::Rgb(0x45, 0x47, 0x5a),
    border_dim: Color::Rgb(0x2a, 0x2a, 0x2a),
    fg: Color::Rgb(0xd4, 0xd4, 0xd4),
    fg_dim: Color::Rgb(0x58, 0x5b, 0x70),
    fg_muted: Color::Rgb(0xa6, 0xad, 0xc8),
    blue: Color::Rgb(0x89, 0xb4, 0xfa),
    cyan: Color::Rgb(0x89, 0xdc, 0xeb),
    green: Color::Rgb(0xa6, 0xe3, 0xa1),
    purple: Color::Rgb(0xcb, 0xa6, 0xf7),
    orange: Color::Rgb(0xfa, 0xb3, 0x87),
    red: Color::Rgb(0xf3, 0x8b, 0xa8),
    code_bg: Color::Rgb(0x24, 0x24, 0x24),
    selection: Color::Rgb(0x31, 0x32, 0x44),
    search: Color::Rgb(0xf9, 0xe2, 0xaf),
};

/// **Rosé Pine** — the `main` variant (charvim's "Knew-pines").
pub const ROSE_PINE: Theme = Theme {
    name: "Rosé Pine",
    bg: Color::Rgb(0x19, 0x17, 0x24),
    bg_dark: Color::Rgb(0x1f, 0x1d, 0x2e),
    bg_highlight: Color::Rgb(0x26, 0x23, 0x3a),
    border: Color::Rgb(0x40, 0x3d, 0x52),
    border_dim: Color::Rgb(0x21, 0x20, 0x2e),
    fg: Color::Rgb(0xe0, 0xde, 0xf4),
    fg_dim: Color::Rgb(0x6e, 0x6a, 0x86),
    fg_muted: Color::Rgb(0x90, 0x8c, 0xaa),
    blue: Color::Rgb(0x31, 0x74, 0x8f),
    cyan: Color::Rgb(0x9c, 0xcf, 0xd8),
    green: Color::Rgb(0x95, 0xb1, 0xac),
    purple: Color::Rgb(0xc4, 0xa7, 0xe7),
    orange: Color::Rgb(0xf6, 0xc1, 0x77),
    red: Color::Rgb(0xeb, 0x6f, 0x92),
    code_bg: Color::Rgb(0x1f, 0x1d, 0x2e),
    selection: Color::Rgb(0x40, 0x3d, 0x52),
    search: Color::Rgb(0xf6, 0xc1, 0x77),
};

/// **Gruvbox** — the dark, medium-contrast variant.
pub const GRUVBOX: Theme = Theme {
    name: "Gruvbox",
    bg: Color::Rgb(0x28, 0x28, 0x28),
    bg_dark: Color::Rgb(0x1d, 0x20, 0x21),
    bg_highlight: Color::Rgb(0x3c, 0x38, 0x36),
    border: Color::Rgb(0x50, 0x49, 0x45),
    border_dim: Color::Rgb(0x32, 0x30, 0x2f),
    fg: Color::Rgb(0xeb, 0xdb, 0xb2),
    fg_dim: Color::Rgb(0x92, 0x83, 0x74),
    fg_muted: Color::Rgb(0xa8, 0x99, 0x84),
    blue: Color::Rgb(0x83, 0xa5, 0x98),
    cyan: Color::Rgb(0x8e, 0xc0, 0x7c),
    green: Color::Rgb(0xb8, 0xbb, 0x26),
    purple: Color::Rgb(0xd3, 0x86, 0x9b),
    orange: Color::Rgb(0xfe, 0x80, 0x19),
    red: Color::Rgb(0xfb, 0x49, 0x34),
    code_bg: Color::Rgb(0x32, 0x30, 0x2f),
    selection: Color::Rgb(0x50, 0x49, 0x45),
    search: Color::Rgb(0xfa, 0xbd, 0x2f),
};

/// **Habamax** — a muted, near-monochrome dark theme (Vim built-in).
pub const HABAMAX: Theme = Theme {
    name: "Habamax",
    bg: Color::Rgb(0x1c, 0x1c, 0x1c),
    bg_dark: Color::Rgb(0x12, 0x12, 0x12),
    bg_highlight: Color::Rgb(0x30, 0x30, 0x30),
    border: Color::Rgb(0x44, 0x44, 0x44),
    border_dim: Color::Rgb(0x26, 0x26, 0x26),
    fg: Color::Rgb(0xbc, 0xbc, 0xbc),
    fg_dim: Color::Rgb(0x6c, 0x6c, 0x6c),
    fg_muted: Color::Rgb(0x9e, 0x9e, 0x9e),
    blue: Color::Rgb(0x89, 0x8e, 0xc7),
    cyan: Color::Rgb(0x6a, 0x9a, 0x9a),
    green: Color::Rgb(0x91, 0xa0, 0x5f),
    purple: Color::Rgb(0xa9, 0x8c, 0xc7),
    orange: Color::Rgb(0xc9, 0x9a, 0x5f),
    red: Color::Rgb(0xc3, 0x7f, 0x7f),
    code_bg: Color::Rgb(0x26, 0x26, 0x26),
    selection: Color::Rgb(0x30, 0x30, 0x30),
    search: Color::Rgb(0xc9, 0xa0, 0x5f),
};

/// Every selectable theme, in command-palette / picker order. The first is the
/// default (Terminal, so ctrlvim follows the terminal until told otherwise).
pub const ALL: &[Theme] =
    &[TERMINAL, TOKYO_NIGHT, CATPPUCCIN, NOIR_CAT, ROSE_PINE, GRUVBOX, HABAMAX];

static ACTIVE: RwLock<Theme> = RwLock::new(TERMINAL);

/// The active theme.
pub fn current() -> Theme {
    *ACTIVE.read().unwrap()
}

/// Swap the active theme wholesale.
pub fn set(theme: Theme) {
    *ACTIVE.write().unwrap() = theme;
}

/// Switch to the theme named `name` (case-insensitive). Returns whether a theme
/// by that name exists.
pub fn set_by_name(name: &str) -> bool {
    match ALL.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
        Some(theme) => {
            set(*theme);
            true
        }
        None => false,
    }
}

/// A foreground that stays readable on top of any accent-colored background —
/// the cursor block, an icon chip, a selected badge.
///
/// `theme::bg()` looks like the natural choice for inverse video, and is
/// wrong: under the default Terminal theme it is [`Color::Reset`], which the
/// terminal resolves to its *default foreground* — usually light. The glyph
/// then dissolves into the accent instead of punching out of it, which is
/// exactly how the cursor became invisible. Accents are always opaque and
/// bright, so black reads against all of them regardless of theme.
pub fn on_accent() -> Color {
    Color::Black
}

// Field accessors — the render code reads colors through these so switching the
// active theme reflows the whole UI without threading a `&Theme` everywhere.
pub fn bg() -> Color {
    current().bg
}
pub fn bg_dark() -> Color {
    current().bg_dark
}
pub fn bg_highlight() -> Color {
    current().bg_highlight
}
pub fn border() -> Color {
    current().border
}
pub fn border_dim() -> Color {
    current().border_dim
}
pub fn fg() -> Color {
    current().fg
}
pub fn fg_dim() -> Color {
    current().fg_dim
}
pub fn fg_muted() -> Color {
    current().fg_muted
}
pub fn blue() -> Color {
    current().blue
}
pub fn cyan() -> Color {
    current().cyan
}
pub fn green() -> Color {
    current().green
}
pub fn purple() -> Color {
    current().purple
}
pub fn orange() -> Color {
    current().orange
}
pub fn red() -> Color {
    current().red
}
pub fn code_bg() -> Color {
    current().code_bg
}
pub fn selection() -> Color {
    current().selection
}
pub fn search() -> Color {
    current().search
}

/// Map a markdown [`MdKind`] to a concrete style, the way `glamour`'s dark
/// theme dresses a rendered document — but in the active theme's palette so the
/// live preview stays cohesive with the rest of the editor.
///
/// Headings descend through the accent ramp (H1 brightest) to give the same
/// scannable hierarchy glow renders. Concealed markers ([`MdKind::Marker`])
/// are dimmed rather than hidden on the cursor's line, so editing stays legible.
pub fn md_style(kind: MdKind) -> Style {
    let s = Style::default();
    match kind {
        MdKind::Text => s.fg(fg()),
        MdKind::Heading(1) => s.fg(purple()).add_modifier(Modifier::BOLD),
        MdKind::Heading(2) => s.fg(blue()).add_modifier(Modifier::BOLD),
        MdKind::Heading(3) => s.fg(cyan()).add_modifier(Modifier::BOLD),
        MdKind::Heading(4) => s.fg(green()).add_modifier(Modifier::BOLD),
        MdKind::Heading(5) => s.fg(orange()).add_modifier(Modifier::BOLD),
        MdKind::Heading(_) => s.fg(fg_muted()).add_modifier(Modifier::BOLD),
        MdKind::Bold => s.fg(fg()).add_modifier(Modifier::BOLD),
        MdKind::Italic => s.fg(fg()).add_modifier(Modifier::ITALIC),
        MdKind::BoldItalic => s.fg(fg()).add_modifier(Modifier::BOLD | Modifier::ITALIC),
        MdKind::Code => s.fg(green()).bg(code_bg()),
        MdKind::CodeBlock => s.fg(fg_muted()).bg(code_bg()),
        MdKind::CodeFence => s.fg(fg_dim()).bg(code_bg()),
        MdKind::Quote => s.fg(fg_dim()).add_modifier(Modifier::ITALIC),
        MdKind::QuoteMarker => s.fg(purple()),
        MdKind::ListMarker => s.fg(blue()),
        MdKind::Checkbox(true) => s.fg(green()),
        MdKind::Checkbox(false) => s.fg(fg_dim()),
        MdKind::Link => s.fg(cyan()).add_modifier(Modifier::UNDERLINED),
        MdKind::Rule => s.fg(border()),
        MdKind::Marker => s.fg(fg_dim()),
    }
}

/// Style for a tree-sitter highlight class, in the active theme's palette.
///
/// The classes reuse the palette the rest of the UI already uses (keywords take
/// the same purple as the git branch, types the same cyan as filetypes), so a
/// highlighted buffer looks like part of the theme rather than a second one.
pub fn syn_style(kind: HlKind) -> Style {
    let s = Style::default();
    match kind {
        HlKind::Keyword => s.fg(purple()),
        HlKind::Function => s.fg(blue()),
        HlKind::Macro => s.fg(blue()).add_modifier(Modifier::ITALIC),
        HlKind::Type => s.fg(cyan()),
        HlKind::Constant => s.fg(orange()),
        HlKind::Number => s.fg(orange()),
        HlKind::String => s.fg(green()),
        HlKind::Comment => s.fg(fg_dim()).add_modifier(Modifier::ITALIC),
        HlKind::Attribute => s.fg(orange()),
        HlKind::Operator => s.fg(cyan()),
        HlKind::Punctuation => s.fg(fg_muted()),
        HlKind::Property => s.fg(blue()),
        HlKind::Variable => s.fg(fg()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_terminal_following() {
        // The default theme follows the terminal: primary bg/fg are `Reset`.
        assert_eq!(TERMINAL.bg, Color::Reset);
        assert_eq!(TERMINAL.fg, Color::Reset);
        assert_eq!(ALL[0].name, TERMINAL.name);
    }

    #[test]
    fn roster_matches_charvim() {
        let names: Vec<&str> = ALL.iter().map(|t| t.name).collect();
        for expected in
            ["Terminal (Auto)", "Tokyo Night", "Catppuccin", "Noir-cat", "Rosé Pine", "Gruvbox", "Habamax"]
        {
            assert!(names.contains(&expected), "missing theme {expected:?} in {names:?}");
        }
    }

    #[test]
    fn set_by_name_switches_and_is_case_insensitive() {
        assert!(set_by_name("gruvbox"));
        assert_eq!(current().name, "Gruvbox");
        assert!(set_by_name("Tokyo Night"));
        assert_eq!(current().name, "Tokyo Night");
        assert!(!set_by_name("no-such-theme"));
        assert_eq!(current().name, "Tokyo Night", "an unknown name leaves the theme unchanged");
        // Leave the global back at the default for any other tests in-process.
        set(TERMINAL);
    }
}
