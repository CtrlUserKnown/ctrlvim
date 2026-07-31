//! Real `nvim-lint` linter definitions — the exact command and output shape
//! each linter's own `nvim-lint` entry (`lua/lint/linters/<name>.lua`) uses,
//! ported to native Rust rather than executed as Lua. `:Lint` (in
//! `ctrlvim-tui`) builds the full command from these plus the buffer being
//! linted, and parses the result with the matching parser there.
//!
//! Only the linters already in [`crate::REGISTRY`] are covered; adding
//! another means reading its real `nvim-lint` definition and porting the
//! same two things: the invocation, and the output parser.

/// One linter's shape — everything about it that's *not* specific to the
/// buffer being linted (which supplies the actual file path/content).
pub struct LintDef {
    /// Matches [`crate::Tool::name`] in the tool registry, and the parser
    /// `ctrlvim-tui` selects by name.
    pub name: &'static str,
    pub filetypes: &'static [&'static str],
    pub binary: &'static str,
}

/// `nvim-lint`'s `shellcheck.lua`: JSON1 output on stdout, buffer content on
/// stdin. Real nvim-lint falls back to passing a real filename when the
/// buffer is backed by one (to resolve shellcheck's `SCRIPTDIR` directive) —
/// ctrlvim always pipes stdin instead, a narrow, documented gap (a script
/// that `source`s a sibling file via `SCRIPTDIR` won't resolve it).
pub const SHELLCHECK: LintDef = LintDef { name: "shellcheck", filetypes: &["sh", "bash"], binary: "shellcheck" };

/// `nvim-lint`'s `ruff.lua`: JSON output on stdout, buffer content on stdin,
/// with `--stdin-filename` set to the real buffer path (ruff uses it only
/// for `# noqa`-style per-file config resolution, not to read the file).
pub const RUFF: LintDef = LintDef { name: "ruff", filetypes: &["python"], binary: "ruff" };

pub const LINTERS: &[&LintDef] = &[&SHELLCHECK, &RUFF];

/// The linter `nvim-lint` would run for `filetype`, if any.
pub fn for_filetype(filetype: &str) -> Option<&'static LintDef> {
    LINTERS.iter().copied().find(|d| d.filetypes.contains(&filetype))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_linter_for_a_known_filetype() {
        assert_eq!(for_filetype("python").map(|d| d.name), Some("ruff"));
        assert_eq!(for_filetype("bash").map(|d| d.name), Some("shellcheck"));
        assert_eq!(for_filetype("sh").map(|d| d.name), Some("shellcheck"));
    }

    #[test]
    fn no_linter_for_an_unknown_filetype() {
        assert!(for_filetype("rust").is_none());
    }
}
