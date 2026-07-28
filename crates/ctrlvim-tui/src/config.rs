//! User configuration, read from `~/.config/ctrlvim/config.toml` on startup.
//!
//! ctrlvim is configured in TOML rather than a config *script*. The split that
//! makes this work is declarative-vs-imperative: TOML expresses the wiring —
//! which options are set, which key runs which command, which event triggers
//! which command, which plugins to load — and anything that needs actual logic
//! lives in a plugin that TOML then refers to *by name*.
//!
//! The bridge is the Ex command. A keymap's `rhs` and an autocmd's `command`
//! are ordinary `:` commands, so a plugin contributes behaviour by registering
//! `:Format` (via `:command`) and the config just names it. That keeps the
//! config free of embedded code without losing extensibility.
//!
//! `[options]` is deliberately untyped: every key becomes a `:set` argument, so
//! any option the engine gains works here immediately with no change to this
//! file.
//!
//! Settings here can also be flipped live from the dashboard's Settings tab,
//! which writes the file back via [`Config::save`]. That write goes through
//! `toml_edit`, so a hand-written config keeps its comments, ordering, and
//! formatting — and, critically, keeps the sections this file does not manage.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::icons::IconMode;

/// A key mapping declared in the config (`[[keymap]]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeymapEntry {
    /// Mode the mapping applies in: `n`, `i`, `v`, … Defaults to normal.
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Left-hand side, in `<...>` notation (`<leader>f`, `<C-p>`).
    pub lhs: String,
    /// Right-hand side — keys to replay, usually an Ex command.
    pub rhs: String,
}

/// An autocommand declared in the config (`[[autocmd]]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AutocmdEntry {
    /// Event name, e.g. `BufWritePre`.
    pub event: String,
    /// File pattern the event must match. Defaults to every file.
    #[serde(default = "default_pattern")]
    pub pattern: String,
    /// Ex command to run when it fires.
    pub command: String,
}

/// A plugin declared in the config (`[[plugin]]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PluginEntry {
    /// Display name.
    pub name: String,
    /// Directory holding the plugin. `~` is expanded.
    pub path: String,
    /// Load lazily on this event instead of at startup.
    #[serde(default)]
    pub event: Option<String>,
    /// Set false to keep the plugin declared but not loaded.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_mode() -> String {
    "n".to_string()
}
fn default_pattern() -> String {
    "*".to_string()
}
fn default_true() -> bool {
    true
}

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
    /// Theme name from `[ui] theme`. `None` leaves the persisted choice alone.
    pub theme: Option<String>,
    /// `[options]` flattened into `:set` arguments, in file order — e.g.
    /// `number = true` becomes `number`, `tabstop = 4` becomes `tabstop=4`.
    pub set_args: Vec<String>,
    pub keymaps: Vec<KeymapEntry>,
    pub autocmds: Vec<AutocmdEntry>,
    pub plugins: Vec<PluginEntry>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            drawer: false,
            mouse: true,
            icons: IconMode::Auto,
            theme: None,
            set_args: Vec::new(),
            keymaps: Vec::new(),
            autocmds: Vec::new(),
            plugins: Vec::new(),
        }
    }
}

/// The on-disk shape, before defaults are resolved. Kept separate from
/// [`Config`] so the runtime struct has no `Option`s to unwrap at every use.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    // Legacy top-level keys, still honored so configs written before the
    // sectioned schema keep working untouched.
    drawer: Option<bool>,
    sidebar: Option<bool>,
    mouse: Option<bool>,
    icons: Option<String>,
    nerd_font: Option<String>,
    nerd_fonts: Option<String>,

    #[serde(default)]
    ui: UiSection,
    /// Untyped on purpose — see the module docs.
    #[serde(default)]
    options: std::collections::BTreeMap<String, OptionValue>,
    #[serde(default)]
    keymap: Vec<KeymapEntry>,
    #[serde(default)]
    autocmd: Vec<AutocmdEntry>,
    #[serde(default)]
    plugin: Vec<PluginEntry>,
}

/// The value shapes `[options]` accepts, mirroring what `:set` can express.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
enum OptionValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

#[derive(Debug, Default, Deserialize)]
struct UiSection {
    drawer: Option<bool>,
    sidebar: Option<bool>,
    mouse: Option<bool>,
    icons: Option<String>,
    theme: Option<String>,
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

    /// Parse a config document. A malformed file yields defaults rather than a
    /// hard failure, so a typo never leaves the user without an editor.
    pub fn parse(text: &str) -> Self {
        let raw: RawConfig = match toml_edit::de::from_str(text) {
            Ok(r) => r,
            Err(_) => return Config::default(),
        };
        let d = Config::default();
        // `[ui]` wins over the legacy top-level spelling when both are present.
        Config {
            drawer: raw
                .ui
                .drawer
                .or(raw.ui.sidebar)
                .or(raw.drawer)
                .or(raw.sidebar)
                .unwrap_or(d.drawer),
            mouse: raw.ui.mouse.or(raw.mouse).unwrap_or(d.mouse),
            icons: raw
                .ui
                .icons
                .or(raw.icons)
                .or(raw.nerd_font)
                .or(raw.nerd_fonts)
                .and_then(|v| IconMode::parse(&v))
                .unwrap_or(d.icons),
            theme: raw.ui.theme,
            set_args: set_args_from(&raw.options),
            keymaps: raw.keymap,
            autocmds: raw.autocmd,
            plugins: raw.plugin,
        }
    }

    /// Persist the config back to disk (best-effort; failures are ignored).
    pub fn save(&self) {
        if let Some(path) = Self::path() {
            let _ = self.save_to(&path);
        }
    }

    /// Write the three UI settings back, leaving every other part of the file —
    /// comments, ordering, `[options]`, `[[keymap]]`, … — exactly as it was.
    ///
    /// This is why the write path goes through `toml_edit` rather than
    /// re-serializing [`Config`]: the Settings tab must not be able to delete a
    /// hand-written config just by toggling a checkbox.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        let mut doc = existing
            .parse::<toml_edit::DocumentMut>()
            .unwrap_or_else(|_| Self::template().parse().expect("template is valid TOML"));
        if doc.as_table().is_empty() {
            doc = Self::template().parse().expect("template is valid TOML");
        }

        // Write into `[ui]` if the file already uses it, otherwise keep the
        // legacy top-level keys where the user put them.
        if doc.get("ui").is_some() {
            let ui = &mut doc["ui"];
            ui["drawer"] = toml_edit::value(self.drawer);
            ui["mouse"] = toml_edit::value(self.mouse);
            ui["icons"] = toml_edit::value(self.icons.as_str());
        } else {
            doc["drawer"] = toml_edit::value(self.drawer);
            doc["mouse"] = toml_edit::value(self.mouse);
            doc["icons"] = toml_edit::value(self.icons.as_str());
        }
        std::fs::write(path, doc.to_string())
    }

    /// The starter config written when none exists yet.
    fn template() -> &'static str {
        "# ctrlvim config\n\
         \n\
         # Open the file drawer on startup.\n\
         drawer = false\n\
         \n\
         # Mouse wheel scrolling in the editor. Turn this off to give the\n\
         # wheel back to the terminal's own scrollback.\n\
         mouse = true\n\
         \n\
         # File icons: \"auto\" (Nerd Font glyphs if one is installed),\n\
         # \"nerd\" (always glyphs), or \"text\" (a letter per filetype).\n\
         icons = \"auto\"\n"
    }
}

/// Flatten an `[options]` table into `:set` arguments.
///
/// `true` becomes a bare option name and `false` its `no`-prefixed form, which
/// is exactly what `:set` already understands — so this needs no knowledge of
/// which options exist, and picks up new ones for free.
fn set_args_from(table: &std::collections::BTreeMap<String, OptionValue>) -> Vec<String> {
    table
        .iter()
        .map(|(key, value)| match value {
            OptionValue::Bool(true) => key.clone(),
            OptionValue::Bool(false) => format!("no{key}"),
            OptionValue::Int(n) => format!("{key}={n}"),
            OptionValue::Str(s) => format!("{key}={s}"),
        })
        .collect()
}

/// Expand a leading `~/` (or bare `~`) to `$HOME`; other paths pass through
/// unchanged (resolved relative to the current directory when read).
pub fn expand_tilde(path: &str) -> PathBuf {
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
    fn parses_legacy_top_level_keys() {
        assert!(Config::parse("drawer = true").drawer);
        assert!(!Config::parse("drawer = false").drawer);
        assert!(Config::parse("# my config\nsidebar = true\n").drawer);
    }

    #[test]
    fn parses_ui_section() {
        let cfg = Config::parse("[ui]\ndrawer = true\nmouse = false\ntheme = \"Gruvbox\"\n");
        assert!(cfg.drawer);
        assert!(!cfg.mouse);
        assert_eq!(cfg.theme.as_deref(), Some("Gruvbox"));
    }

    #[test]
    fn parses_icon_mode() {
        assert_eq!(Config::default().icons, IconMode::Auto);
        assert_eq!(Config::parse("icons = \"nerd\"").icons, IconMode::Nerd);
        assert_eq!(Config::parse("icons = \"text\"").icons, IconMode::Text);
        assert_eq!(Config::parse("icons = \"wat\"").icons, IconMode::Auto);
    }

    #[test]
    fn options_become_set_arguments() {
        let cfg = Config::parse(
            "[options]\nnumber = true\nwrap = false\ntabstop = 4\nfoldmethod = \"indent\"\n",
        );
        assert!(cfg.set_args.contains(&"number".to_string()));
        assert!(cfg.set_args.contains(&"nowrap".to_string()), "false → no-prefix");
        assert!(cfg.set_args.contains(&"tabstop=4".to_string()));
        assert!(cfg.set_args.contains(&"foldmethod=indent".to_string()));
    }

    #[test]
    fn parses_keymaps_autocmds_and_plugins() {
        let text = "\
[[keymap]]
lhs = \"<leader>f\"
rhs = \":Files<CR>\"

[[keymap]]
mode = \"i\"
lhs = \"jk\"
rhs = \"<Esc>\"

[[autocmd]]
event = \"BufWritePre\"
pattern = \"*.rs\"
command = \"Format\"

[[plugin]]
name = \"demo\"
path = \"~/.config/ctrlvim/pack/demo\"
event = \"BufWritePre\"
";
        let cfg = Config::parse(text);
        assert_eq!(cfg.keymaps.len(), 2);
        assert_eq!(cfg.keymaps[0].mode, "n", "mode defaults to normal");
        assert_eq!(cfg.keymaps[1].mode, "i");
        assert_eq!(cfg.autocmds.len(), 1);
        assert_eq!(cfg.autocmds[0].pattern, "*.rs");
        assert_eq!(cfg.plugins.len(), 1);
        assert!(cfg.plugins[0].enabled, "plugins are enabled unless told otherwise");
        assert_eq!(cfg.plugins[0].event.as_deref(), Some("BufWritePre"));
    }

    #[test]
    fn an_autocmd_pattern_defaults_to_everything() {
        let cfg = Config::parse("[[autocmd]]\nevent = \"BufEnter\"\ncommand = \"echo hi\"\n");
        assert_eq!(cfg.autocmds[0].pattern, "*");
    }

    #[test]
    fn a_malformed_file_falls_back_to_defaults() {
        assert_eq!(Config::parse("this is not = = toml ["), Config::default());
    }

    #[test]
    fn saving_preserves_sections_it_does_not_manage() {
        // The regression this guards: the Settings tab used to rewrite the file
        // from scratch, which would silently delete a user's keymaps.
        let path = std::env::temp_dir()
            .join(format!("ctrlvim-cfg-{}-{:p}.toml", std::process::id(), &()));
        let original = "\
# keep me
drawer = false
mouse = true
icons = \"auto\"

[options]
number = true

[[keymap]]
lhs = \"<leader>f\"
rhs = \":Files<CR>\"
";
        std::fs::write(&path, original).unwrap();

        let mut cfg = Config::load_from(&path);
        cfg.drawer = true;
        cfg.save_to(&path).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# keep me"), "comments survive");
        assert!(written.contains("[[keymap]]"), "keymaps survive");
        assert!(written.contains("number = true"), "options survive");

        let reloaded = Config::load_from(&path);
        assert!(reloaded.drawer, "the toggled setting was persisted");
        assert_eq!(reloaded.keymaps.len(), 1);
        assert_eq!(reloaded.set_args, vec!["number".to_string()]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_creates_a_starter_file_when_none_exists() {
        let path = std::env::temp_dir()
            .join(format!("ctrlvim-new-{}-{:p}.toml", std::process::id(), &()));
        let _ = std::fs::remove_file(&path);
        let cfg = Config { drawer: true, ..Config::default() };
        cfg.save_to(&path).unwrap();
        assert!(Config::load_from(&path).drawer);
        let _ = std::fs::remove_file(&path);
    }
}
