//! File icons: Nerd Font glyphs, with a plain-text fallback.
//!
//! Every file icon carries two renderings — a Nerd Font glyph (``, ``, …)
//! and a plain letter (`R`, `M`, …) — and the chip picks one at render time
//! from the config's [`IconMode`]. That keeps the choice live-switchable from
//! the dashboard's Settings tab instead of being baked in when entries are
//! scanned.
//!
//! In [`IconMode::Auto`] (the default) we probe the host for an installed Nerd
//! Font once, since a terminal can't tell us whether its font has the Private
//! Use Area glyphs: `fc-list` first, then a shallow scan of the usual font
//! directories, with `CTRLVIM_NERD_FONT=1|0` as an explicit override.

use std::path::Path;
use std::sync::OnceLock;

/// How file icons are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconMode {
    /// Use glyphs if a Nerd Font looks installed, else extension text.
    #[default]
    Auto,
    /// Always use Nerd Font glyphs.
    Nerd,
    /// Always use extension text.
    Text,
}

impl IconMode {
    pub fn as_str(self) -> &'static str {
        match self {
            IconMode::Auto => "auto",
            IconMode::Nerd => "nerd",
            IconMode::Text => "text",
        }
    }

    /// Parse a config value (`auto` / `nerd` / `text`, plus a few aliases).
    pub fn parse(s: &str) -> Option<IconMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "detect" => Some(IconMode::Auto),
            "nerd" | "nerdfont" | "nerd_font" | "glyph" | "true" | "on" => Some(IconMode::Nerd),
            "text" | "ascii" | "extension" | "ext" | "false" | "off" => Some(IconMode::Text),
            _ => None,
        }
    }

    /// The next mode in the Settings-row cycle.
    pub fn next(self) -> IconMode {
        match self {
            IconMode::Auto => IconMode::Nerd,
            IconMode::Nerd => IconMode::Text,
            IconMode::Text => IconMode::Auto,
        }
    }

    /// Label for the Settings row — `auto` spells out what it resolved to.
    pub fn label(self) -> String {
        match self {
            IconMode::Auto => {
                format!("auto ({})", if detected() { "nerd" } else { "text" })
            }
            other => other.as_str().to_string(),
        }
    }

    /// Whether icons render as Nerd Font glyphs under this mode.
    pub fn nerd(self) -> bool {
        match self {
            IconMode::Nerd => true,
            IconMode::Text => false,
            IconMode::Auto => detected(),
        }
    }
}

/// Whether a Nerd Font looks installed on this host (probed once, then cached).
pub fn detected() -> bool {
    static DETECTED: OnceLock<bool> = OnceLock::new();
    *DETECTED.get_or_init(detect)
}

fn detect() -> bool {
    if let Some(forced) = env_override() {
        return forced;
    }
    if let Some(found) = fontconfig_has_nerd_font() {
        return found;
    }
    font_dirs_have_nerd_font()
}

/// `CTRLVIM_NERD_FONT=1|0` (or true/false) forces the answer, for hosts where
/// the font is installed somewhere we don't look.
fn env_override() -> Option<bool> {
    let raw = std::env::var("CTRLVIM_NERD_FONT").ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Ask fontconfig. `None` means fontconfig isn't available, so the caller
/// should fall back to scanning directories.
fn fontconfig_has_nerd_font() -> Option<bool> {
    let out = std::process::Command::new("fc-list")
        .arg(":family")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let families = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
    Some(families.contains("nerd"))
}

/// Shallow scan of the usual font directories for a Nerd Font file name
/// (`JetBrainsMonoNerdFont-Regular.ttf`, `MesloLGS NF Regular.ttf`, …).
fn font_dirs_have_nerd_font() -> bool {
    let home = std::env::var("HOME").ok();
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(data) = std::env::var("XDG_DATA_HOME") {
        dirs.push(Path::new(&data).join("fonts"));
    }
    if let Some(home) = &home {
        let home = Path::new(home);
        dirs.push(home.join(".local/share/fonts"));
        dirs.push(home.join(".fonts"));
        dirs.push(home.join("Library/Fonts"));
    }
    dirs.push("/usr/share/fonts".into());
    dirs.push("/usr/local/share/fonts".into());
    dirs.push("/Library/Fonts".into());
    dirs.iter().any(|d| dir_has_nerd_font(d, 3))
}

/// Look for a Nerd Font file under `dir`, descending at most `depth` levels and
/// giving up after a bounded number of entries so startup never stalls on a
/// huge font tree.
fn dir_has_nerd_font(dir: &Path, depth: u32) -> bool {
    const MAX_ENTRIES: u32 = 4000;
    let mut budget = MAX_ENTRIES;
    let mut stack = vec![(dir.to_path_buf(), depth)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for entry in read.flatten() {
            if budget == 0 {
                return false;
            }
            budget -= 1;
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if is_font_dir_entry(&entry) {
                if depth > 0 {
                    stack.push((entry.path(), depth - 1));
                }
                continue;
            }
            if is_nerd_font_file(&name) {
                return true;
            }
        }
    }
    false
}

fn is_font_dir_entry(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
}

/// True for a font file whose name marks it as Nerd-Font-patched.
pub(crate) fn is_nerd_font_file(name: &str) -> bool {
    let is_font = name.ends_with(".ttf")
        || name.ends_with(".otf")
        || name.ends_with(".ttc")
        || name.ends_with(".woff2");
    is_font && (name.contains("nerd") || name.contains(" nf ") || name.contains("nf-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_cycles_modes() {
        assert_eq!(IconMode::parse("auto"), Some(IconMode::Auto));
        assert_eq!(IconMode::parse("Nerd"), Some(IconMode::Nerd));
        assert_eq!(IconMode::parse("ext"), Some(IconMode::Text));
        assert_eq!(IconMode::parse("wat"), None);
        assert_eq!(IconMode::Auto.next(), IconMode::Nerd);
        assert_eq!(IconMode::Nerd.next(), IconMode::Text);
        assert_eq!(IconMode::Text.next(), IconMode::Auto);
    }

    #[test]
    fn explicit_modes_bypass_detection() {
        assert!(IconMode::Nerd.nerd());
        assert!(!IconMode::Text.nerd());
        // Auto agrees with whatever the host probe found.
        assert_eq!(IconMode::Auto.nerd(), detected());
    }

    #[test]
    fn recognizes_nerd_font_file_names() {
        assert!(is_nerd_font_file("jetbrainsmononerdfont-regular.ttf"));
        assert!(is_nerd_font_file("symbolsnerdfont-regular.otf"));
        assert!(is_nerd_font_file("meslolgs nf regular.ttf"));
        assert!(!is_nerd_font_file("dejavusans.ttf"));
        // Not a font file, even if the name mentions it.
        assert!(!is_nerd_font_file("nerd-fonts.txt"));
    }
}
