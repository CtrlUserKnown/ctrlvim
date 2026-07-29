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

use ctrlvim_ai::AiConfig;
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
    /// What the mapping does, shown by `?` and the which-key popup. Optional,
    /// but a described mapping is the difference between a discoverable
    /// keymap and one you have to read the config to understand.
    #[serde(default)]
    pub desc: Option<String>,
}

/// A user command declared in config (`[[command]]`).
///
/// The bridge that lets a keymap do something the engine has no built-in for:
/// `expansion` is an ordinary Ex command line, so `!python3 %:p` plus a
/// `[[keymap]]` pointing at it is a complete "run this file" binding with no
/// Rust involved.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CommandEntry {
    /// What to type after the colon, e.g. `RunPython`.
    pub name: String,
    /// The Ex command line it stands for.
    pub expansion: String,
}

/// A built-in mapping to remove (`[[unmap]]`).
///
/// Config can already *shadow* a default by binding the same lhs, but there
/// was no way to get rid of one — so a chord you never use kept its key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UnmapEntry {
    #[serde(default = "default_mode")]
    pub mode: String,
    pub lhs: String,
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
///
/// Not `Eq`: the `[ai]` sampling knobs are floats.
#[derive(Debug, Clone, PartialEq)]
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
    /// Built-in mappings to remove, from `[[unmap]]`.
    pub unmaps: Vec<UnmapEntry>,
    /// User commands from `[[command]]`.
    pub commands: Vec<CommandEntry>,
    /// Whether to install the built-in mappings at all (`[keymaps] defaults`).
    /// `false` starts from an empty table, for a config that wants to define
    /// every chord itself.
    pub keymap_defaults: bool,
    /// `mapleader` — what `<leader>` expands to in `lhs`/`rhs` (`[keymaps] leader`).
    pub leader: char,
    pub autocmds: Vec<AutocmdEntry>,
    pub plugins: Vec<PluginEntry>,
    /// Inline AI completion, from `[ai]` / `[ai.model]`.
    pub ai: AiConfig,
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
            unmaps: Vec::new(),
            commands: Vec::new(),
            keymap_defaults: true,
            leader: ' ',
            autocmds: Vec::new(),
            plugins: Vec::new(),
            ai: AiConfig::default(),
        }
    }
}

/// The `[ai]` section, before defaults are resolved.
///
/// Every field is optional and mirrors a field of [`AiConfig`], so a config
/// that just says `[ai]\nenabled = true` gets sensible everything-else. The
/// enum-ish fields (`device`, `precision`) are strings here and are parsed
/// leniently: an unrecognized value falls back to the default rather than
/// costing the user their editor.
#[derive(Debug, Default, Deserialize)]
struct AiSection {
    enabled: Option<bool>,
    device: Option<String>,
    debounce_ms: Option<u64>,
    max_tokens: Option<usize>,
    max_millis: Option<u64>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    repeat_penalty: Option<f32>,
    repeat_last_n: Option<usize>,
    seed: Option<u64>,
    context_before: Option<usize>,
    context_after: Option<usize>,
    max_prefix_chars: Option<usize>,
    max_suffix_chars: Option<usize>,
    max_lines: Option<usize>,
    threads: Option<usize>,
    #[serde(default)]
    model: AiModelSection,
}

/// The `[ai.model]` sub-section: which weights to load.
#[derive(Debug, Default, Deserialize)]
struct AiModelSection {
    repo: Option<String>,
    revision: Option<String>,
    /// A local directory of weights. `~` is expanded, and setting it skips the
    /// Hugging Face download entirely.
    path: Option<String>,
    precision: Option<String>,
    /// Quantized weights: `owner/repo:file.gguf`, or a path to a `.gguf`.
    /// The empty string means "no quantization — use the safetensors".
    gguf: Option<String>,
}

impl AiSection {
    fn resolve(self) -> AiConfig {
        let d = AiConfig::default();
        let m = self.model;
        AiConfig {
            enabled: self.enabled.unwrap_or(d.enabled),
            model: ctrlvim_ai::ModelSource {
                repo: m.repo.unwrap_or(d.model.repo),
                revision: m.revision.unwrap_or(d.model.revision),
                path: m.path.as_deref().map(expand_tilde),
                precision: m
                    .precision
                    .as_deref()
                    .and_then(ctrlvim_ai::Precision::parse)
                    .unwrap_or(d.model.precision),
                // Quantization is on by default, so an *explicit* empty string
                // has to be able to turn it off — `GgufSource::parse` returns
                // `None` for one, and only an absent key falls back.
                gguf: match m.gguf.as_deref() {
                    Some(spec) => ctrlvim_ai::GgufSource::parse(spec),
                    None => d.model.gguf,
                },
            },
            device: self
                .device
                .as_deref()
                .and_then(ctrlvim_ai::DevicePref::parse)
                .unwrap_or(d.device),
            debounce_ms: self.debounce_ms.unwrap_or(d.debounce_ms),
            max_tokens: self.max_tokens.unwrap_or(d.max_tokens),
            max_millis: self.max_millis.unwrap_or(d.max_millis),
            temperature: self.temperature.unwrap_or(d.temperature),
            // `top_p = 0` is how a config file says "no nucleus sampling";
            // candle would otherwise take it literally and mask every token.
            top_p: self.top_p.filter(|p| *p > 0.0 && *p < 1.0),
            repeat_penalty: self.repeat_penalty.unwrap_or(d.repeat_penalty),
            repeat_last_n: self.repeat_last_n.unwrap_or(d.repeat_last_n),
            seed: self.seed.unwrap_or(d.seed),
            context_before: self.context_before.unwrap_or(d.context_before),
            context_after: self.context_after.unwrap_or(d.context_after),
            max_prefix_chars: self.max_prefix_chars.unwrap_or(d.max_prefix_chars),
            max_suffix_chars: self.max_suffix_chars.unwrap_or(d.max_suffix_chars),
            max_lines: self.max_lines.unwrap_or(d.max_lines),
            threads: self.threads.or(d.threads),
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
    unmap: Vec<UnmapEntry>,
    #[serde(default)]
    command: Vec<CommandEntry>,
    #[serde(default)]
    keymaps: KeymapsSection,
    #[serde(default)]
    autocmd: Vec<AutocmdEntry>,
    #[serde(default)]
    plugin: Vec<PluginEntry>,
    #[serde(default)]
    ai: AiSection,
}

/// The `[keymaps]` section — settings *about* the mapping table, as opposed to
/// the `[[keymap]]` entries that populate it.
#[derive(Debug, Deserialize)]
struct KeymapsSection {
    /// Install the built-in mappings. `false` starts from an empty table.
    #[serde(default = "default_true")]
    defaults: bool,
    /// What `<leader>` expands to. A string so `" "` is writable in TOML;
    /// only the first character is used.
    #[serde(default)]
    leader: Option<String>,
}

impl Default for KeymapsSection {
    fn default() -> Self {
        KeymapsSection { defaults: true, leader: None }
    }
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
            unmaps: raw.unmap,
            commands: raw.command,
            keymap_defaults: raw.keymaps.defaults,
            // Only the first character counts; `leader = ""` keeps Space
            // rather than making `<leader>` unmatchable.
            leader: raw.keymaps.leader.and_then(|s| s.chars().next()).unwrap_or(d.leader),
            autocmds: raw.autocmd,
            plugins: raw.plugin,
            ai: raw.ai.resolve(),
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

        // `[ai]` always lives in its own table — there is no legacy top-level
        // spelling to preserve. Only `enabled` is written: the model, device,
        // and sampling knobs have no UI, so rewriting them here would flatten a
        // hand-tuned section into whatever the defaults happen to be.
        if !doc.get("ai").is_some_and(|item| item.is_table()) {
            doc["ai"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        doc["ai"]["enabled"] = toml_edit::value(self.ai.enabled);

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
         icons = \"auto\"\n\
         \n\
         # Inline AI code suggestions (CodeGemma-2B, running locally).\n\
         # The first use downloads ~1.6GB of 4-bit quantized weights.\n\
         # Slow on a CPU; see docs/ai.md.\n\
         # [ai]\n\
         # enabled = true\n\
         # device = \"auto\"       # auto | cpu | cuda | cuda:1 | metal\n\
         # debounce_ms = 350\n\
         # max_tokens = 64\n\
         #\n\
         # [ai.model]\n\
         # repo = \"google/codegemma-2b\"\n\
         # precision = \"bf16\"    # bf16 | f16 | f32\n\
         # path = \"~/models/codegemma-2b\"  # skip the download entirely\n"
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
    fn a_keymap_carries_its_description() {
        // `desc` is the only source of the text `?` and the which-key popup
        // show, so it has to survive parsing.
        let cfg = Config::parse(
            "[[keymap]]\nlhs = \"<leader>w\"\nrhs = \":w<CR>\"\ndesc = \"Save file\"\n",
        );
        assert_eq!(cfg.keymaps[0].desc.as_deref(), Some("Save file"));
        // ...and is optional.
        let cfg = Config::parse("[[keymap]]\nlhs = \"x\"\nrhs = \"y\"\n");
        assert_eq!(cfg.keymaps[0].desc, None);
    }

    #[test]
    fn keymap_defaults_and_leader_are_configurable() {
        let d = Config::default();
        assert!(d.keymap_defaults, "built-ins are installed unless told otherwise");
        assert_eq!(d.leader, ' ');

        let cfg = Config::parse("[keymaps]\ndefaults = false\nleader = \",\"\n");
        assert!(!cfg.keymap_defaults);
        assert_eq!(cfg.leader, ',');

        // An empty leader keeps Space rather than making `<leader>` unmatchable.
        assert_eq!(Config::parse("[keymaps]\nleader = \"\"\n").leader, ' ');
    }

    #[test]
    fn parses_unmaps_and_user_commands() {
        let text = "\
[[unmap]]
lhs = \"<leader>d\"

[[unmap]]
mode = \"v\"
lhs = \"<A-j>\"

[[command]]
name = \"RunPython\"
expansion = \"!python3 %:p\"
";
        let cfg = Config::parse(text);
        assert_eq!(cfg.unmaps.len(), 2);
        assert_eq!(cfg.unmaps[0].mode, "n", "mode defaults to normal");
        assert_eq!(cfg.unmaps[1].mode, "v");
        assert_eq!(cfg.commands.len(), 1);
        assert_eq!(cfg.commands[0].name, "RunPython");
        assert_eq!(cfg.commands[0].expansion, "!python3 %:p");
    }

    #[test]
    fn an_autocmd_pattern_defaults_to_everything() {
        let cfg = Config::parse("[[autocmd]]\nevent = \"BufEnter\"\ncommand = \"echo hi\"\n");
        assert_eq!(cfg.autocmds[0].pattern, "*");
    }

    #[test]
    fn the_shipped_example_config_parses() {
        // `Config::parse` falls back to defaults on a malformed file, silently.
        // That is the right behaviour at runtime and a trap for the example we
        // tell users to copy: a typo in it would look exactly like an empty
        // config. So assert on values it actually sets.
        let text = include_str!("../../../docs/config.example.toml");
        let cfg = Config::parse(text);
        assert_ne!(cfg, Config::default(), "the example parsed as nothing at all");

        assert_eq!(cfg.theme.as_deref(), Some("Tokyo Night"));
        assert!(cfg.set_args.contains(&"timeoutlen=500".to_string()));
        assert!(cfg.keymap_defaults);
        assert_eq!(cfg.leader, ' ');
        assert!(
            cfg.keymaps.iter().all(|m| m.desc.is_some()),
            "every example mapping should model describing itself"
        );
        assert!(cfg.keymaps.iter().any(|m| m.lhs == "<leader>g"));
    }

    #[test]
    fn every_example_mapping_is_actually_installable() {
        // The example is the first thing a user copies, so a mapping in it that
        // the engine rejects would be a bad first impression.
        let cfg = Config::parse(include_str!("../../../docs/config.example.toml"));
        let mut km = ctrlvim_core::Keymap::new();
        km.set_leader(cfg.leader);
        for m in &cfg.keymaps {
            km.set_with_desc(
                ctrlvim_core::MapMode::parse(&m.mode),
                &m.lhs,
                &m.rhs,
                m.desc.clone(),
            )
            .unwrap_or_else(|e| panic!("example mapping {:?} is not installable: {e}", m.lhs));
        }
    }

    #[test]
    fn a_malformed_file_falls_back_to_defaults() {
        assert_eq!(Config::parse("this is not = = toml ["), Config::default());
    }

    #[test]
    fn ai_is_off_unless_the_config_asks_for_it() {
        // Downloading gigabytes of weights must be something the user chose.
        assert!(!Config::default().ai.enabled);
        assert!(!Config::parse("[ui]\nmouse = true\n").ai.enabled);
        assert!(Config::parse("[ai]\nenabled = true\n").ai.enabled);
    }

    #[test]
    fn ai_settings_override_only_what_they_mention() {
        let cfg = Config::parse(
            "[ai]\nenabled = true\ndevice = \"cpu\"\nmax_tokens = 16\n\n\
             [ai.model]\nprecision = \"f32\"\n",
        );
        let d = ctrlvim_ai::AiConfig::default();
        assert_eq!(cfg.ai.device, ctrlvim_ai::DevicePref::Cpu);
        assert_eq!(cfg.ai.max_tokens, 16);
        assert_eq!(cfg.ai.model.precision, ctrlvim_ai::Precision::F32);
        assert_eq!(cfg.ai.model.repo, d.model.repo, "untouched keys keep defaults");
        assert_eq!(cfg.ai.debounce_ms, d.debounce_ms);
    }

    #[test]
    fn an_unparseable_ai_enum_falls_back_instead_of_failing() {
        // A typo in `device` must not cost the user the whole config file.
        let cfg = Config::parse("[ai]\nenabled = true\ndevice = \"quantum\"\n");
        assert!(cfg.ai.enabled);
        assert_eq!(cfg.ai.device, ctrlvim_ai::AiConfig::default().device);
    }

    #[test]
    fn a_local_model_path_is_tilde_expanded() {
        let cfg = Config::parse("[ai.model]\npath = \"~/models/codegemma\"\n");
        let path = cfg.ai.model.path.expect("a path was given");
        assert!(!path.starts_with("~"), "got {}", path.display());
        assert!(path.ends_with("models/codegemma"));
    }

    #[test]
    fn a_zero_top_p_means_no_nucleus_sampling() {
        // candle reads `top_p` literally, and 0.0 would mask every token — so
        // the "unset" spelling has to be normalized away here.
        assert_eq!(Config::parse("[ai]\ntop_p = 0.0\n").ai.top_p, None);
        assert_eq!(Config::parse("[ai]\ntop_p = 0.9\n").ai.top_p, Some(0.9));
        assert_eq!(Config::parse("[ai]\ntop_p = 1.0\n").ai.top_p, None);
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
    fn toggling_ai_persists_without_flattening_the_rest_of_the_section() {
        // The Settings checkbox owns `enabled` and nothing else: a hand-tuned
        // model/device/token budget must survive being toggled off and on.
        let path = std::env::temp_dir()
            .join(format!("ctrlvim-ai-cfg-{}-{:p}.toml", std::process::id(), &()));
        let original = "\
[ai]
enabled = false
max_tokens = 128

[ai.model]
repo = \"unsloth/codegemma-2b\"
precision = \"f16\"
";
        std::fs::write(&path, original).unwrap();

        let mut cfg = Config::load_from(&path);
        cfg.ai.enabled = true;
        cfg.save_to(&path).unwrap();

        let reloaded = Config::load_from(&path);
        assert!(reloaded.ai.enabled, "the toggle was persisted");
        assert_eq!(reloaded.ai.max_tokens, 128, "hand-tuned budget survives");
        assert_eq!(reloaded.ai.model.repo, "unsloth/codegemma-2b");
        assert_eq!(reloaded.ai.model.precision, ctrlvim_ai::Precision::F16);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn toggling_ai_creates_the_section_when_the_file_has_none() {
        let path = std::env::temp_dir()
            .join(format!("ctrlvim-ai-new-{}-{:p}.toml", std::process::id(), &()));
        std::fs::write(&path, "# my config\ndrawer = true\n").unwrap();

        let mut cfg = Config::load_from(&path);
        cfg.ai.enabled = true;
        cfg.save_to(&path).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# my config"), "comments survive:\n{written}");
        assert!(Config::load_from(&path).ai.enabled);
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
