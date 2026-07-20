//! Runtime domain types for the dashboard.
//!
//! Unlike the design prototype, these hold **real** data gathered from the
//! project the editor is launched in (see [`crate::data`]) rather than static
//! mock constants. The one exception is [`KEYBINDINGS`], which is in-app
//! documentation copy, not project data.

use ratatui::style::Color;

use crate::theme;

/// A recent file / explorer entry / openable buffer source.
#[derive(Clone)]
pub struct FileEntry {
    pub name: String,
    /// Path relative to the project root.
    pub path: String,
    pub icon_color: Color,
    pub icon_letter: char,
    pub modified: String,
}

/// A previously-opened project.
#[derive(Clone)]
pub struct SessionEntry {
    pub name: String,
    pub branch: String,
    pub files: u32,
    pub last: String,
}

/// Plugin lifecycle status; carries its own display color + label.
#[derive(Clone, Copy, PartialEq)]
pub enum PluginStatus {
    Loaded,
    Update,
    Lazy,
}

impl PluginStatus {
    pub fn color(self) -> Color {
        match self {
            PluginStatus::Loaded => theme::GREEN,
            PluginStatus::Update => theme::ORANGE,
            PluginStatus::Lazy => theme::FG_DIM,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PluginStatus::Loaded => "loaded",
            PluginStatus::Update => "update",
            PluginStatus::Lazy => "lazy",
        }
    }
}

#[derive(Clone)]
pub struct Plugin {
    pub name: String,
    pub repo: String,
    pub category: String,
    pub status: PluginStatus,
}

/// A language server row in the Settings tab. `installed` reflects whether the
/// server binary was found on `PATH`; the live on/off toggle lives in
/// [`crate::app::App`].
#[derive(Clone)]
pub struct LspServer {
    pub name: String,
    pub filetypes: String,
    pub installed: bool,
}

/// Real git status for the project root, or `None` when it isn't a repo.
#[derive(Clone)]
pub struct GitStatus {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub modified: u32,
    pub staged: u32,
    pub remote: String,
    pub last_commit: String,
    pub untracked: u32,
}

/// Startup / project stats.
#[derive(Clone)]
pub struct Stats {
    pub startup_ms: u128,
    pub plugins_loaded: usize,
    pub plugins_total: usize,
    /// Lines of code, already thousands-grouped for display.
    pub loc: String,
}

/// In-app keybinding documentation copy (distinct from the real keymap).
pub struct Keybind {
    pub keys: &'static str,
    pub desc: &'static str,
}

pub const KEYBINDINGS: &[Keybind] = &[
    Keybind { keys: "<leader>ff", desc: "Find file" },
    Keybind { keys: "<leader>fg", desc: "Live grep" },
    Keybind { keys: "<leader>e",  desc: "Toggle explorer" },
    Keybind { keys: "gd",         desc: "Go to definition" },
    Keybind { keys: ":Lazy",      desc: "Plugin manager" },
    Keybind { keys: "<C-p>",      desc: "Command palette" },
];

/// Pick a colored icon chip (letter + color) for a file, by extension.
pub fn icon_for(name: &str) -> (char, Color) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" => ('R', theme::ORANGE),
        "toml" => ('T', theme::FG_DIM),
        "md" | "markdown" => ('M', theme::BLUE),
        "lua" => ('L', theme::BLUE),
        "js" | "mjs" | "cjs" => ('J', theme::ORANGE),
        "ts" | "tsx" => ('T', theme::CYAN),
        "json" => ('J', theme::ORANGE),
        "py" => ('P', theme::GREEN),
        "sh" | "bash" | "zsh" => ('S', theme::GREEN),
        "yaml" | "yml" => ('Y', theme::PURPLE),
        "html" => ('H', theme::ORANGE),
        "css" => ('C', theme::BLUE),
        _ => {
            let c = name
                .chars()
                .find(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('•');
            (c, theme::FG_DIM)
        }
    }
}
