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
    pub icon: FileIcon,
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

/// A tool row in the Settings tab — a language server, formatter, linter, or
/// build linker. `installed` reflects whether a binary was found at all
/// (either on `PATH` or in ctrlvim's own tools directory, see `managed`); the
/// live on/off toggle lives in [`crate::app::App`].
#[derive(Clone)]
pub struct LspServer {
    pub name: String,
    pub filetypes: String,
    pub installed: bool,
    /// `installed` was found in ctrlvim's own tools directory rather than on
    /// `PATH` — i.e. ctrlvim installed it itself.
    pub managed: bool,
    /// ctrlvim knows how to install this one (see `ctrlvim_tools::REGISTRY`).
    pub installable: bool,
}

/// One path git reports as not matching HEAD, with how it differs.
///
/// Kept alongside the counts so `[c]` can fill the quickfix list without
/// shelling out again — the porcelain output the counts came from already
/// named every file.
#[derive(Clone)]
pub struct GitChange {
    /// Repo-relative, exactly as git printed it.
    pub path: String,
    /// `conflict` / `staged` / `modified` / `untracked`, for the quickfix text.
    pub label: &'static str,
}

/// Real git status for the project root, or `None` when it isn't a repo.
#[derive(Clone)]
pub struct GitStatus {
    pub branch: String,
    /// Abbreviated HEAD commit, or `—` before the first commit.
    pub oid: String,
    pub ahead: u32,
    pub behind: u32,
    pub modified: u32,
    pub staged: u32,
    pub remote: String,
    /// Relative time of the last commit (`%cr`).
    pub last_commit: String,
    pub last_author: String,
    pub last_subject: String,
    pub untracked: u32,
    /// Files with unresolved merge conflicts (porcelain-v2 `u` entries).
    ///
    /// These are counted separately because git reports them as neither staged
    /// nor modified: a conflicted tree would otherwise show `0 / 0` and read as
    /// clean at exactly the moment it is least clean.
    pub conflicted: u32,
    pub stashes: u32,
    /// Lines added / removed against HEAD, for the `+N −M` summary.
    pub insertions: u32,
    pub deletions: u32,
    pub changed: Vec<GitChange>,
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

/// A file's icon chip: a Nerd Font glyph plus the single-letter fallback shown
/// on terminals without one (see [`crate::icons`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIcon {
    /// Nerd Font glyph (Private Use Area).
    pub glyph: char,
    /// Fallback letter — the first letter of the file's extension.
    pub letter: char,
    pub color: Color,
}

impl FileIcon {
    pub fn new(glyph: char, letter: char, color: Color) -> Self {
        FileIcon { glyph, letter, color }
    }

    /// The chip label under `mode`.
    pub fn label(&self, mode: crate::icons::IconMode) -> char {
        if mode.nerd() {
            self.glyph
        } else {
            self.letter
        }
    }

    /// The icon for a directory row.
    pub fn dir() -> Self {
        FileIcon::new(FOLDER, '/', theme::blue())
    }
}

// Nerd Font code points (v3), the same set nvim-web-devicons draws from.
const FOLDER: char = '\u{e5ff}';
const FILE: char = '\u{f15b}';
const FILE_TEXT: char = '\u{f15c}';
const FILE_IMAGE: char = '\u{f1c5}';
const FILE_PDF: char = '\u{f1c1}';
const FILE_ARCHIVE: char = '\u{f1c6}';
const CONFIG: char = '\u{e615}';
const TERMINAL: char = '\u{e795}';

/// Pick the icon (glyph + letter fallback + color) for a file, by extension.
pub fn icon_for(name: &str) -> FileIcon {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let (glyph, color) = match ext.as_str() {
        "rs" => ('\u{e7a8}', theme::orange()),
        "toml" | "ini" | "cfg" | "conf" => (CONFIG, theme::fg_dim()),
        "yaml" | "yml" => (CONFIG, theme::purple()),
        "md" | "markdown" => ('\u{e73e}', theme::blue()),
        "lua" => ('\u{e620}', theme::blue()),
        "vim" => ('\u{e62b}', theme::green()),
        "js" | "mjs" | "cjs" => ('\u{e60c}', theme::orange()),
        "ts" => ('\u{e628}', theme::cyan()),
        "tsx" | "jsx" => ('\u{e7ba}', theme::cyan()),
        "json" => ('\u{e60b}', theme::orange()),
        "py" => ('\u{e73c}', theme::green()),
        "sh" | "bash" | "zsh" | "fish" => (TERMINAL, theme::green()),
        "html" | "htm" => ('\u{e736}', theme::orange()),
        "css" => ('\u{e749}', theme::blue()),
        "scss" | "sass" => ('\u{e603}', theme::purple()),
        "go" => ('\u{e627}', theme::cyan()),
        "c" | "h" => ('\u{e61e}', theme::blue()),
        "cpp" | "cc" | "hpp" | "cxx" => ('\u{e61d}', theme::blue()),
        "java" => ('\u{e738}', theme::red()),
        "rb" => ('\u{e739}', theme::red()),
        "php" => ('\u{e73d}', theme::purple()),
        "gitignore" | "gitattributes" | "gitmodules" => ('\u{e702}', theme::orange()),
        "dockerfile" | "dockerignore" => ('\u{e7b0}', theme::blue()),
        "txt" | "log" => (FILE_TEXT, theme::fg_dim()),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" => {
            (FILE_IMAGE, theme::purple())
        }
        "pdf" => (FILE_PDF, theme::red()),
        "zip" | "gz" | "tar" | "xz" | "zst" | "7z" => (FILE_ARCHIVE, theme::orange()),
        _ => (FILE, theme::fg_dim()),
    };
    FileIcon::new(glyph, fallback_letter(name, &ext), color)
}

/// The letter shown when there's no Nerd Font: the extension's first letter, or
/// — for an extension-less file like `Makefile` — the name's.
fn fallback_letter(name: &str, ext: &str) -> char {
    let from = if ext.is_empty() { name } else { ext };
    from.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('•')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_get_a_glyph_and_a_letter_fallback() {
        let rs = icon_for("main.rs");
        assert_eq!(rs.glyph, '\u{e7a8}');
        assert_eq!(rs.letter, 'R');
        assert_eq!(icon_for("README.md").letter, 'M');
        assert_eq!(icon_for("Cargo.toml").letter, 'T');
        assert_eq!(icon_for("init.lua").letter, 'L');
        assert_eq!(icon_for("pkg.json").letter, 'J');
        // Dotfiles are matched on the part after the dot.
        assert_eq!(icon_for(".gitignore").glyph, '\u{e702}');
    }

    #[test]
    fn unknown_extensions_get_the_generic_file_glyph() {
        let icon = icon_for("notes.wibble");
        assert_eq!(icon.glyph, FILE);
        assert_eq!(icon.letter, 'W');
    }

    #[test]
    fn extensionless_files_fall_back_to_the_names_first_letter() {
        assert_eq!(icon_for("Makefile").letter, 'M');
        assert_eq!(icon_for("LICENSE").letter, 'L');
        assert_eq!(icon_for("...").letter, '•');
    }

    #[test]
    fn the_chip_label_follows_the_icon_mode() {
        use crate::icons::IconMode;
        let icon = icon_for("main.rs");
        assert_eq!(icon.label(IconMode::Nerd), '\u{e7a8}');
        assert_eq!(icon.label(IconMode::Text), 'R');
    }
}
