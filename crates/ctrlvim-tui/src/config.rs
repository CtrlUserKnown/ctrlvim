//! User configuration, read from `~/.config/ctrlvim/config.toml` on startup.
//!
//! ctrlvim has no config crate dependency, so this parses the small subset of
//! TOML the config actually uses — `key = value` lines (`true`/`false`, quoted
//! strings, integers), `# comments`, and `[section]` headers (which are
//! accepted and ignored; keys are matched by name). Anything unrecognized is
//! skipped, so an over-rich file never breaks startup.
//!
//! Settings here can also be flipped live from the dashboard's Settings tab,
//! which writes the file back via [`Config::save`].

use std::path::{Path, PathBuf};

/// Parsed user configuration. Every field has a sensible default so a missing
/// or partial file still yields a usable config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Open the file drawer (the `Ctrl+B` sidebar) automatically on startup.
    pub drawer: bool,
    /// Enable mouse support in the editor (scrolling to move through the buffer).
    pub mouse: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { drawer: false, mouse: false }
    }
}

impl Config {
    /// Path of the config file (`$XDG_CONFIG_HOME/ctrlvim/config.toml`).
    pub fn path() -> Option<PathBuf> {
        crate::data::config_dir().map(|c| c.join("ctrlvim").join("config.toml"))
    }

    /// Load the config, falling back to defaults if the file is absent or
    /// unreadable.
    pub fn load() -> Self {
        match Self::path() {
            Some(path) => Self::load_from(&path),
            None => Config::default(),
        }
    }

    /// Load from a specific path (defaults if absent/unreadable).
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Config::default(),
        }
    }

    /// Persist the config back to disk (best-effort; failures are ignored).
    pub fn save(&self) {
        if let Some(path) = Self::path() {
            let _ = self.save_to(&path);
        }
    }

    /// Serialize and write to a specific path, creating parent dirs.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_toml())
    }

    /// Render the config as a TOML document.
    fn to_toml(&self) -> String {
        format!(
            "# ctrlvim config\n\n\
             # Open the file drawer on startup.\n\
             drawer = {}\n\n\
             # Enable mouse support (scroll the editor).\n\
             mouse = {}\n",
            self.drawer, self.mouse
        )
    }

    /// Parse the TOML subset described in the module docs.
    pub fn parse(text: &str) -> Self {
        let mut cfg = Config::default();
        for line in text.lines() {
            // Strip comments and skip blanks / section headers.
            let line = match line.find('#') {
                Some(i) => &line[..i],
                None => line,
            };
            let line = line.trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "drawer" | "sidebar" => {
                    if let Some(b) = parse_bool(value) {
                        cfg.drawer = b;
                    }
                }
                "mouse" => {
                    if let Some(b) = parse_bool(value) {
                        cfg.mouse = b;
                    }
                }
                _ => {}
            }
        }
        cfg
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hides_the_drawer() {
        assert!(!Config::default().drawer);
    }

    #[test]
    fn parses_drawer_option() {
        assert!(Config::parse("drawer = true").drawer);
        assert!(!Config::parse("drawer = false").drawer);
        // Alias, plus a comment and a section header are tolerated.
        let text = "# my config\n[ui]\nsidebar = true # open it\n";
        assert!(Config::parse(text).drawer);
    }

    #[test]
    fn parses_mouse_option() {
        assert!(Config::parse("mouse = true").mouse);
        assert!(!Config::parse("mouse = false").mouse);
        assert!(!Config::default().mouse);
    }

    #[test]
    fn unknown_keys_and_junk_are_ignored() {
        let text = "wat = 3\nnonsense line\ndrawer = true\n";
        assert!(Config::parse(text).drawer);
        // Missing key → default.
        assert!(!Config::parse("theme = \"Gruvbox\"").drawer);
    }

    #[test]
    fn save_load_roundtrip() {
        // Hermetic: writes to a unique temp file, never the real config dir.
        let path = std::env::temp_dir()
            .join(format!("ctrlvim-cfg-{}-{:p}.toml", std::process::id(), &()));
        let cfg = Config { drawer: true, mouse: true };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        // A missing file loads defaults.
        let _ = std::fs::remove_file(&path);
        assert_eq!(Config::load_from(&path), Config::default());
    }
}
