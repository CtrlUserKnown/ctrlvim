//! The "Mason"-equivalent piece of the tool story: a built-in registry of
//! language servers, formatters, and linters, plus enough of an installer to
//! fetch them into a ctrlvim-owned directory without the user reaching for
//! `rustup component add` / `npm i -g` / a system package manager by hand.
//!
//! This crate deliberately knows nothing about LSP-the-protocol or the TUI —
//! it only answers two questions: "is this tool available, and where", and
//! "what command installs it". Everything else (spawning that command,
//! wiring an actual LSP client to the resulting binary) lives in `ctrlvim-tui`
//! and a future `ctrlvim-lsp`, respectively.

pub mod lint;

use std::path::{Path, PathBuf};

/// What a [`Tool`] is used for, mirroring the three things Mason manages
/// (build linkers, which ctrlvim also lists in this space, aren't part of
/// this registry — they're system packages, not per-project dev tools).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Lsp,
    Formatter,
    Linter,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Lsp => "lsp",
            Category::Formatter => "fmt",
            Category::Linter => "lint",
        }
    }
}

/// How to fetch a tool. Each variant maps to a well-known package ecosystem
/// so the install command is a single, unsurprising line — the same command
/// a human would type from that ecosystem's own docs, just aimed at
/// ctrlvim's own tools directory instead of a global location.
#[derive(Clone, Copy)]
pub enum InstallMethod {
    /// `cargo install --root <dir> <args>` — `args` is everything after the
    /// crate name that install needs (extra features, `--locked`, etc.).
    Cargo(&'static str),
    /// `npm install --prefix <dir> <packages...>`.
    Npm(&'static [&'static str]),
    /// `pip3 install --prefix <dir> <package>`.
    Pip(&'static str),
    /// `GOBIN=<dir>/bin go install <module>@version`.
    GoInstall(&'static str),
    /// `rustup component add <component>` — installs alongside the active
    /// toolchain (already on `PATH` via rustup's shims), not into ctrlvim's
    /// tools directory.
    Rustup(&'static str),
    /// No known scriptable install yet; the user installs it themselves.
    Unsupported,
}

impl InstallMethod {
    pub fn is_supported(self) -> bool {
        !matches!(self, InstallMethod::Unsupported)
    }

    /// The shell command line that installs this tool into `dir`. `dir` is
    /// ignored by [`InstallMethod::Rustup`] and unused (returns `None`) for
    /// [`InstallMethod::Unsupported`].
    pub fn command(self, dir: &Path) -> Option<String> {
        let dir = dir.display();
        match self {
            InstallMethod::Cargo(args) => Some(format!("cargo install --root '{dir}' {args}")),
            InstallMethod::Npm(pkgs) => {
                Some(format!("npm install --prefix '{dir}' {}", pkgs.join(" ")))
            }
            InstallMethod::Pip(pkg) => Some(format!("pip3 install --prefix '{dir}' {pkg}")),
            InstallMethod::GoInstall(module) => {
                Some(format!("GOBIN='{dir}/bin' go install {module}"))
            }
            InstallMethod::Rustup(component) => Some(format!("rustup component add {component}")),
            InstallMethod::Unsupported => None,
        }
    }
}

/// One installable tool: a language server, formatter, or linter ctrlvim
/// knows the shape of.
pub struct Tool {
    /// Matches the nvim-lspconfig identifier for LSP servers, so the same
    /// config vocabulary carries over.
    pub name: &'static str,
    pub filetypes: &'static str,
    pub category: Category,
    /// Candidate binary names to look for, in order — some tools install
    /// under more than one plausible name.
    pub binaries: &'static [&'static str],
    pub method: InstallMethod,
}

/// The known tools ctrlvim can detect and, where supported, install.
pub const REGISTRY: &[Tool] = &[
    Tool {
        name: "rust_analyzer",
        filetypes: "rust",
        category: Category::Lsp,
        binaries: &["rust-analyzer"],
        method: InstallMethod::Rustup("rust-analyzer"),
    },
    Tool {
        name: "taplo",
        filetypes: "toml",
        category: Category::Lsp,
        binaries: &["taplo"],
        method: InstallMethod::Cargo("taplo-cli --locked --features lsp"),
    },
    Tool {
        name: "lua_ls",
        filetypes: "lua",
        category: Category::Lsp,
        binaries: &["lua-language-server", "lua_ls"],
        method: InstallMethod::Unsupported,
    },
    Tool {
        name: "marksman",
        filetypes: "markdown",
        category: Category::Lsp,
        binaries: &["marksman"],
        method: InstallMethod::Unsupported,
    },
    Tool {
        name: "ts_ls",
        filetypes: "ts, tsx, js, jsx",
        category: Category::Lsp,
        binaries: &["typescript-language-server", "tsserver"],
        method: InstallMethod::Npm(&["typescript-language-server", "typescript"]),
    },
    Tool {
        name: "jdtls",
        filetypes: "java (maven/gradle)",
        category: Category::Lsp,
        binaries: &["jdtls"],
        method: InstallMethod::Unsupported,
    },
    Tool {
        name: "lemminx",
        filetypes: "xml (pom.xml)",
        category: Category::Lsp,
        binaries: &["lemminx"],
        method: InstallMethod::Unsupported,
    },
    Tool {
        name: "mesonlsp",
        filetypes: "meson",
        category: Category::Lsp,
        binaries: &["mesonlsp", "Swift-MesonLSP"],
        method: InstallMethod::Unsupported,
    },
    Tool {
        name: "stylua",
        filetypes: "lua",
        category: Category::Formatter,
        binaries: &["stylua"],
        method: InstallMethod::Cargo("stylua"),
    },
    Tool {
        name: "prettier",
        filetypes: "ts, js, css, html, md",
        category: Category::Formatter,
        binaries: &["prettier"],
        method: InstallMethod::Npm(&["prettier"]),
    },
    Tool {
        name: "shfmt",
        filetypes: "sh, bash",
        category: Category::Formatter,
        binaries: &["shfmt"],
        method: InstallMethod::GoInstall("mvdan.cc/sh/v3/cmd/shfmt@latest"),
    },
    Tool {
        name: "shellcheck",
        filetypes: "sh, bash",
        category: Category::Linter,
        binaries: &["shellcheck"],
        method: InstallMethod::Unsupported,
    },
    Tool {
        name: "ruff",
        filetypes: "python",
        category: Category::Linter,
        binaries: &["ruff"],
        method: InstallMethod::Pip("ruff"),
    },
];

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
}

/// `$XDG_DATA_HOME`, falling back to `$HOME/.local/share`.
fn data_home() -> Option<PathBuf> {
    data_home_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

fn data_home_from(xdg_data_home: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    xdg_data_home
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home.map(|h| PathBuf::from(h).join(".local").join("share")))
}

/// Where ctrlvim keeps everything it installs itself: one subdirectory per
/// tool, so removing a tool is just `rm -rf` on its directory.
pub fn tools_root() -> Option<PathBuf> {
    data_home().map(|d| d.join("ctrlvim").join("tools"))
}

/// The directory a specific tool is (or would be) installed into.
pub fn install_dir(name: &str) -> Option<PathBuf> {
    tools_root().map(|r| r.join(name))
}

/// The `bin/` directory a specific tool's binaries land in, once installed —
/// the directory to prepend to `PATH` before spawning it.
pub fn bin_dir(name: &str) -> Option<PathBuf> {
    install_dir(name).map(|d| d.join("bin"))
}

/// Where a tool was found, if anywhere.
pub enum Location {
    /// Already on `PATH` — nothing ctrlvim installed.
    System,
    /// Found in ctrlvim's own tools directory, at this path.
    Managed(PathBuf),
}

/// Look for `tool`'s binary on `PATH` first, then in ctrlvim's own tools
/// directory.
pub fn locate(tool: &Tool) -> Option<Location> {
    if tool.binaries.iter().any(|b| on_path(b)) {
        return Some(Location::System);
    }
    let dir = bin_dir(tool.name)?;
    tool.binaries
        .iter()
        .map(|b| dir.join(b))
        .find(|p| p.is_file())
        .map(Location::Managed)
}

/// Convenience over [`locate`] for callers that just want the two booleans a
/// display row needs: whether it's installed at all, and whether that
/// install is one ctrlvim manages itself.
pub fn installed_and_managed(tool: &Tool) -> (bool, bool) {
    match locate(tool) {
        Some(Location::System) => (true, false),
        Some(Location::Managed(_)) => (true, true),
        None => (false, false),
    }
}

/// The shell command that installs `tool` into its own tools directory, or
/// `None` when [`InstallMethod::Unsupported`] or there's nowhere to put it
/// (no resolvable `$HOME`).
pub fn install_command(tool: &Tool) -> Option<String> {
    match tool.method {
        InstallMethod::Unsupported => None,
        InstallMethod::Rustup(_) => tool.method.command(Path::new("")),
        _ => tool.method.command(&install_dir(tool.name)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_are_unique() {
        let mut names: Vec<&str> = REGISTRY.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names, deduped, "duplicate tool name in the registry");
    }

    #[test]
    fn every_registry_entry_has_at_least_one_binary_name() {
        for tool in REGISTRY {
            assert!(!tool.binaries.is_empty(), "{} has no candidate binaries", tool.name);
        }
    }

    #[test]
    fn cargo_method_builds_a_root_scoped_install_command() {
        let cmd = InstallMethod::Cargo("stylua").command(Path::new("/home/x/tools/stylua")).unwrap();
        assert_eq!(cmd, "cargo install --root '/home/x/tools/stylua' stylua");
    }

    #[test]
    fn npm_method_joins_multiple_packages() {
        let cmd = InstallMethod::Npm(&["a", "b"]).command(Path::new("/d")).unwrap();
        assert_eq!(cmd, "npm install --prefix '/d' a b");
    }

    #[test]
    fn go_install_method_sets_gobin_to_the_dirs_bin_subdirectory() {
        let cmd = InstallMethod::GoInstall("example.com/cmd/foo@latest").command(Path::new("/d")).unwrap();
        assert_eq!(cmd, "GOBIN='/d/bin' go install example.com/cmd/foo@latest");
    }

    #[test]
    fn rustup_method_ignores_the_directory() {
        let cmd = InstallMethod::Rustup("rust-analyzer").command(Path::new("/unused")).unwrap();
        assert_eq!(cmd, "rustup component add rust-analyzer");
    }

    #[test]
    fn unsupported_method_has_no_command() {
        assert!(InstallMethod::Unsupported.command(Path::new("/d")).is_none());
        let tool =
            Tool { name: "x", filetypes: "", category: Category::Lsp, binaries: &["x"], method: InstallMethod::Unsupported };
        assert!(install_command(&tool).is_none());
    }

    #[test]
    fn data_home_prefers_xdg_data_home_when_set() {
        let dir = data_home_from(Some("/xdg/data".into()), Some("/home/x".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/data"));
    }

    #[test]
    fn data_home_falls_back_to_home_local_share() {
        let dir = data_home_from(None, Some("/home/x".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/x/.local/share"));
    }

    #[test]
    fn data_home_ignores_an_empty_xdg_data_home() {
        let dir = data_home_from(Some("".into()), Some("/home/x".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/x/.local/share"));
    }

    #[test]
    fn data_home_is_none_without_either_variable() {
        assert!(data_home_from(None, None).is_none());
    }
}
