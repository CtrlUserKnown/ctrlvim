//! `lsp.lua`: the single source of truth for which language servers and
//! build linkers exist, read once at startup.
//!
//! This mirrors the role `config.toml`'s `[[plugin]]` array plays for
//! plugins: the compiled editor has zero built-in knowledge of any specific
//! server or linker — no name, no filetype mapping, no executable path, no
//! install command lives in Rust source. Every one of those comes from the
//! user's own `lsp.lua`, and a server this file doesn't declare does not
//! exist anywhere in the editor: not in the Settings tab, not spawned, not
//! counted, not even shown as "not found".
//!
//! The file is a plain Lua expression, not a scripting session — no `vim.*`
//! authoring API is exposed here (that's `ctrlvim-lua`'s job for plugins).
//! It just has to evaluate to a table shaped like:
//!
//! ```lua
//! return {
//!   { name = "rust_analyzer", filetypes = { "rust" }, cmd = { "rust-analyzer" } },
//!   {
//!     name = "ts_ls",
//!     filetypes = { "typescript", "javascript", "tsx" },
//!     cmd = { "typescript-language-server", "--stdio" },
//!     install = "npm install -g typescript-language-server typescript",
//!   },
//!   -- A build linker: no `filetypes`, so it's never spawned as a language
//!   -- server, just checked for presence and shown as a status row.
//!   { name = "mold", cmd = { "mold" } },
//! }
//! ```

use std::path::{Path, PathBuf};

use mlua::{Lua, LuaSerdeExt};
use serde::Deserialize;

use crate::model::LspServer;

/// One declared server or linker — a row `lsp.lua`'s returned table
/// contributes. `name` is arbitrary (there's no registry to match against);
/// it's just what the Settings tab and `I` install action call it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LspServerDecl {
    pub name: String,
    /// Filetype names (matching `ctrlvim_core::Filetype::name()`, e.g.
    /// `"rust"`, `"typescript"`) this server attaches to. Empty means
    /// "presence-only" — the entry gets a status row and can be installed,
    /// but nothing ever spawns it as a language server (the shape a build
    /// linker declaration wants).
    #[serde(default)]
    pub filetypes: Vec<String>,
    /// The program to run plus its LSP-mode arguments — `cmd[0]` is the
    /// binary looked up on `PATH`, `cmd[1..]` are passed as-is (e.g.
    /// `["typescript-language-server", "--stdio"]`). Also what a
    /// presence-only entry's `cmd[0]` is checked against. Required: there's
    /// no sensible "installed?" check without a binary name.
    pub cmd: Vec<String>,
    /// A shell command line that installs it, run verbatim by the Settings
    /// tab's `I` action. The editor never inspects or knows what it does —
    /// that knowledge is entirely the user's.
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Path of `lsp.lua` (`$XDG_CONFIG_HOME/ctrlvim/lsp.lua`).
pub fn path() -> Option<PathBuf> {
    crate::data::config_dir().map(|c| c.join("ctrlvim").join("lsp.lua"))
}

/// Load every declared server/linker from the real `lsp.lua`. No file at all
/// means nothing was declared — an empty list, not an error. A file that
/// fails to run or doesn't evaluate to the expected shape also yields an
/// empty list, plus the error for the caller to surface; it never crashes
/// the editor and never falls back to a guessed default.
pub fn load() -> (Vec<LspServerDecl>, Option<String>) {
    match path() {
        Some(p) => load_from(&p),
        None => (Vec::new(), None),
    }
}

/// [`load`] against a specific path — split out so it's testable without
/// depending on the real `$XDG_CONFIG_HOME`.
pub fn load_from(path: &Path) -> (Vec<LspServerDecl>, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (Vec::new(), None),
    };
    parse(&text, path)
}

/// Evaluate `text` as `lsp.lua`'s body and deserialize the result. `path` is
/// only used to name the chunk for error messages.
fn parse(text: &str, path: &Path) -> (Vec<LspServerDecl>, Option<String>) {
    let lua = Lua::new();
    let value = match lua.load(text).set_name(path.display().to_string()).eval::<mlua::Value>() {
        Ok(v) => v,
        Err(e) => return (Vec::new(), Some(format!("lsp.lua: {e}"))),
    };
    match lua.from_value::<Vec<LspServerDecl>>(value) {
        Ok(decls) => (decls, None),
        Err(e) => {
            (Vec::new(), Some(format!("lsp.lua: expected `return {{ {{ name = ..., cmd = {{...}} }}, ... }}`: {e}")))
        }
    }
}

/// Build the Settings tab's display rows from the declared list — one row
/// per declared entry, in declared order, `installed` a live `PATH` check of
/// `cmd[0]`. Nothing here is ever added or removed based on what the
/// compiled editor happens to know about; it only reflects what's here.
pub fn to_display(decls: &[LspServerDecl]) -> Vec<LspServer> {
    decls
        .iter()
        .map(|d| LspServer {
            name: d.name.clone(),
            filetypes: d.filetypes.join(", "),
            installed: d.cmd.first().is_some_and(|bin| crate::data::locate(&d.name, bin).is_some()),
            install: d.install.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_declares_nothing_and_is_not_an_error() {
        let (decls, err) = load_from(Path::new("/nonexistent/ctrlvim-lsp-does-not-exist.lua"));
        assert!(decls.is_empty());
        assert!(err.is_none());
    }

    #[test]
    fn parses_a_declared_server() {
        let (decls, err) = parse(
            r#"
            return {
              {
                name = "rust_analyzer",
                filetypes = { "rust" },
                cmd = { "rust-analyzer" },
              },
            }
            "#,
            Path::new("lsp.lua"),
        );
        assert!(err.is_none(), "unexpected error: {err:?}");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, "rust_analyzer");
        assert_eq!(decls[0].filetypes, vec!["rust".to_string()]);
        assert_eq!(decls[0].cmd, vec!["rust-analyzer".to_string()]);
        assert_eq!(decls[0].install, None);
        assert!(decls[0].enabled, "enabled defaults to true");
    }

    #[test]
    fn parses_an_install_command_and_multiple_filetypes() {
        let (decls, _) = parse(
            r#"
            return {
              {
                name = "ts_ls",
                filetypes = { "typescript", "javascript", "tsx" },
                cmd = { "typescript-language-server", "--stdio" },
                install = "npm install -g typescript-language-server typescript",
              },
            }
            "#,
            Path::new("lsp.lua"),
        );
        assert_eq!(decls[0].filetypes, vec!["typescript", "javascript", "tsx"]);
        assert_eq!(decls[0].cmd, vec!["typescript-language-server", "--stdio"]);
        assert_eq!(decls[0].install.as_deref(), Some("npm install -g typescript-language-server typescript"));
    }

    #[test]
    fn a_presence_only_entry_has_no_filetypes() {
        let (decls, _) = parse(r#"return { { name = "mold", cmd = { "mold" } } }"#, Path::new("lsp.lua"));
        assert!(decls[0].filetypes.is_empty());
    }

    #[test]
    fn enabled_false_is_honored() {
        let (decls, _) =
            parse(r#"return { { name = "x", cmd = { "x" }, enabled = false } }"#, Path::new("lsp.lua"));
        assert!(!decls[0].enabled);
    }

    #[test]
    fn a_lua_syntax_error_yields_an_empty_list_and_an_error_message_not_a_crash() {
        let (decls, err) = parse("this is not } valid lua [[[", Path::new("lsp.lua"));
        assert!(decls.is_empty());
        assert!(err.is_some());
    }

    #[test]
    fn a_table_missing_required_fields_yields_an_empty_list_and_an_error() {
        // `cmd` has a `#[serde(default)]` so it's actually optional, but a
        // missing `name` is not — this must fail cleanly, not panic.
        let (decls, err) = parse(r#"return { { cmd = { "x" } } }"#, Path::new("lsp.lua"));
        assert!(decls.is_empty());
        assert!(err.is_some());
    }

    #[test]
    fn a_return_that_is_not_a_list_of_tables_yields_an_empty_list_and_an_error() {
        let (decls, err) = parse(r#"return "not a table""#, Path::new("lsp.lua"));
        assert!(decls.is_empty());
        assert!(err.is_some());
    }

    #[test]
    fn to_display_joins_filetypes_and_carries_the_install_command() {
        let decls = vec![LspServerDecl {
            name: "ts_ls".into(),
            filetypes: vec!["typescript".into(), "javascript".into()],
            cmd: vec!["ctrlvim-definitely-not-a-real-binary".into(), "--stdio".into()],
            install: Some("npm install -g typescript-language-server".into()),
            enabled: true,
        }];
        let rows = to_display(&decls);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "ts_ls");
        assert_eq!(rows[0].filetypes, "typescript, javascript");
        assert!(!rows[0].installed, "a made-up binary name must not be found on PATH");
        assert_eq!(rows[0].install.as_deref(), Some("npm install -g typescript-language-server"));
    }

    #[test]
    fn to_display_is_empty_when_nothing_was_declared() {
        assert!(to_display(&[]).is_empty());
    }

    #[test]
    fn the_shipped_example_lsp_lua_parses() {
        // `parse` falls back to an empty list on any error, silently — the
        // right behavior at runtime, but a trap for the example we tell
        // users to copy: a typo in it would look exactly like an empty file.
        // Assert on values it actually declares.
        let text = include_str!("../../../docs/lsp.example.lua");
        let (decls, err) = parse(text, Path::new("lsp.example.lua"));
        assert!(err.is_none(), "the shipped example failed to parse: {err:?}");
        assert!(!decls.is_empty(), "the shipped example declared nothing at all");

        let ra = decls.iter().find(|d| d.name == "rust_analyzer").expect("rust_analyzer in the example");
        assert_eq!(ra.filetypes, vec!["rust".to_string()]);
        assert_eq!(ra.cmd, vec!["rust-analyzer".to_string()]);
        assert!(ra.install.is_some());

        let mold = decls.iter().find(|d| d.name == "mold").expect("mold in the example");
        assert!(mold.filetypes.is_empty(), "mold is a presence-only (build linker) entry");
    }
}
