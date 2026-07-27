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
//!
//! `plugin = "path"` lines declare Lua scripts to run once at startup, in the
//! order they appear, over the same `:luafile` path ([`crate::app::App`]'s
//! `host_source`). Repeat the key for more than one plugin; there's no array
//! syntax in this parser. Paths are resolved relative to the current
//! directory unless absolute, and a leading `~/` expands to `$HOME`.

use std::path::{Path, PathBuf};

use crate::icons::IconMode;

/// Parsed user configuration. Every field has a sensible default so a missing
/// or partial file still yields a usable config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Open the file drawer (the `Ctrl+B` sidebar) automatically on startup.
    pub drawer: bool,
    /// Enable mouse support in the editor (wheel scrolling). On by default, as
    /// in Neovim since 0.8; set `mouse = false` to give the wheel back to the
    /// terminal for its own scrollback.
    pub mouse: bool,
    /// How file icons are drawn: Nerd Font glyphs, extension text, or auto-
    /// detect (see [`crate::icons`]).
    pub icons: IconMode,
    /// Lua scripts to run once at startup, in file order (`plugin = "path"`
    /// lines). Empty by default — ctrlvim loads nothing unless asked.
    pub plugins: Vec<PathBuf>,
    /// The shell used to run external commands (`:!`, `:make`-style jobs via
    /// `sh -c`-style invocation). Defaults to `zsh` on macOS and `bash`
    /// everywhere else, overridden with `shell = "fish"` in `config.toml`.
    pub shell: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            drawer: false,
            mouse: true,
            icons: IconMode::Auto,
            plugins: Vec::new(),
            shell: default_shell(),
        }
    }
}

/// The shell to fall back to when `config.toml` doesn't set one: `zsh` on
/// macOS (its login default since Catalina), `bash` elsewhere.
fn default_shell() -> String {
    if cfg!(target_os = "macos") { "zsh".to_string() } else { "bash".to_string() }
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
        let mut text = format!(
            "# ctrlvim config\n\n\
             # Open the file drawer on startup.\n\
             drawer = {}\n\n\
             # Mouse wheel scrolling in the editor. Turn this off to give the\n\
             # wheel back to the terminal's own scrollback.\n\
             mouse = {}\n\n\
             # File icons: \"auto\" (Nerd Font glyphs if one is installed),\n\
             # \"nerd\" (always glyphs), or \"text\" (a letter per filetype).\n\
             icons = \"{}\"\n\n\
             # Shell used to run external commands. Defaults to zsh on macOS,\n\
             # bash elsewhere.\n\
             shell = \"{}\"\n",
            self.drawer,
            self.mouse,
            self.icons.as_str(),
            self.shell,
        );
        if !self.plugins.is_empty() {
            text.push_str(
                "\n# Lua plugins to run once at startup, in order (one `plugin = \"path\"`\n\
                 # line each; see examples/plugins/ for a sample).\n",
            );
            for path in &self.plugins {
                text.push_str(&format!("plugin = \"{}\"\n", path.display()));
            }
        }
        text
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
                "icons" | "nerd_font" | "nerd_fonts" => {
                    if let Some(m) = IconMode::parse(value) {
                        cfg.icons = m;
                    }
                }
                "plugin" => {
                    if !value.is_empty() {
                        cfg.plugins.push(expand_tilde(value));
                    }
                }
                "shell" => {
                    if !value.is_empty() {
                        cfg.shell = value.to_string();
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

/// Expand a leading `~/` (or bare `~`) to `$HOME`; other paths pass through
/// unchanged (resolved relative to the current directory when read).
fn expand_tilde(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() => crate::data::home().unwrap_or_else(|| PathBuf::from("~")),
        Some(rest) if rest.starts_with('/') => match crate::data::home() {
            Some(home) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(path),
        },
        _ => PathBuf::from(path),
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
        assert!(Config::default().mouse, "on by default, as in Neovim");
    }

    #[test]
    fn parses_icon_mode() {
        assert_eq!(Config::default().icons, IconMode::Auto);
        assert_eq!(Config::parse("icons = \"nerd\"").icons, IconMode::Nerd);
        assert_eq!(Config::parse("icons = \"text\"").icons, IconMode::Text);
        // Alias, and an unparseable value keeps the default.
        assert_eq!(Config::parse("nerd_font = true").icons, IconMode::Nerd);
        assert_eq!(Config::parse("icons = \"wat\"").icons, IconMode::Auto);
    }

    #[test]
    fn parses_plugin_lines_in_order() {
        assert!(Config::default().plugins.is_empty());
        let text = "plugin = \"a.lua\"\nplugin = \"/abs/b.lua\"\n";
        let plugins = Config::parse(text).plugins;
        assert_eq!(plugins, vec![PathBuf::from("a.lua"), PathBuf::from("/abs/b.lua")]);
    }

    #[test]
    fn expands_tilde_in_plugin_paths() {
        // Reads the real $HOME rather than overriding it, so this can't race
        // with other tests that read the same process-wide env var.
        let home = crate::data::home().expect("HOME must be set to run this test");
        assert_eq!(
            Config::parse("plugin = \"~/plugins/hello.lua\"").plugins,
            vec![home.join("plugins/hello.lua")]
        );
        // A bare `~` (no trailing slash) also resolves.
        assert_eq!(Config::parse("plugin = \"~\"").plugins, vec![home]);
    }

    #[test]
    fn shell_defaults_by_platform() {
        let expected = if cfg!(target_os = "macos") { "zsh" } else { "bash" };
        assert_eq!(Config::default().shell, expected);
    }

    #[test]
    fn parses_shell_option() {
        assert_eq!(Config::parse("shell = \"fish\"").shell, "fish");
        // Empty value keeps the platform default rather than clearing it.
        assert_eq!(Config::parse("shell = \"\"").shell, Config::default().shell);
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
        let cfg = Config {
            drawer: true,
            mouse: true,
            icons: IconMode::Text,
            plugins: vec![PathBuf::from("a.lua"), PathBuf::from("/abs/b.lua")],
            shell: "fish".to_string(),
        };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        // A missing file loads defaults.
        let _ = std::fs::remove_file(&path);
        assert_eq!(Config::load_from(&path), Config::default());
    }
}
