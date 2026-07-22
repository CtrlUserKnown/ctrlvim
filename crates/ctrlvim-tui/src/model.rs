//! Runtime domain types for the dashboard.
//!
//! Unlike the design prototype, these hold **real** data gathered from the
//! project the editor is launched in (see [`crate::data`]) rather than static
//! mock constants.

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
            PluginStatus::Loaded => theme::green(),
            PluginStatus::Update => theme::orange(),
            PluginStatus::Lazy => theme::fg_dim(),
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

/// Pick a colored icon chip (letter + color) for a file, by extension.
pub fn icon_for(name: &str) -> (char, Color) {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" => ('R', theme::orange()),
        "toml" => ('T', theme::fg_dim()),
        "md" | "markdown" => ('M', theme::blue()),
        "lua" => ('L', theme::blue()),
        "js" | "mjs" | "cjs" => ('J', theme::orange()),
        "ts" | "tsx" => ('T', theme::cyan()),
        "json" => ('J', theme::orange()),
        "py" => ('P', theme::green()),
        "sh" | "bash" | "zsh" => ('S', theme::green()),
        "yaml" | "yml" => ('Y', theme::purple()),
        "html" => ('H', theme::orange()),
        "css" => ('C', theme::blue()),
        _ => {
            let c = name
                .chars()
                .find(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('•');
            (c, theme::fg_dim())
        }
    }
}
