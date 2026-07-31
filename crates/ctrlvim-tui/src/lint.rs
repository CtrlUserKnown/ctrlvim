//! `:Lint` — builds the exact command a real `nvim-lint` setup would run for
//! a given linter, and parses its output the same way `nvim-lint`'s own
//! parser does. See `ctrlvim_tools::lint` for which linter runs for which
//! filetype; this module is the buffer-specific half (the command needs the
//! file being linted, the parser needs to turn JSON into [`QfItem`]s).

use ctrlvim_core::{QfItem, QfKind};
use ctrlvim_tools::lint::LintDef;
use std::path::PathBuf;

/// The command line `nvim-lint`'s own definition for `def` would run,
/// against `buffer_path` (used only where the real definition uses the
/// buffer's name, e.g. ruff's `--stdin-filename`, not to read the file —
/// the content always goes over stdin, written by the caller).
pub fn build_command(def: &LintDef, buffer_path: &str) -> (&'static str, Vec<String>) {
    match def.name {
        "shellcheck" => (
            def.binary,
            vec!["--format".to_string(), "json1".to_string(), "-".to_string()],
        ),
        "ruff" => (
            def.binary,
            vec![
                "check".to_string(),
                "--force-exclude".to_string(),
                "--quiet".to_string(),
                "--stdin-filename".to_string(),
                buffer_path.to_string(),
                "--no-fix".to_string(),
                "--output-format".to_string(),
                "json".to_string(),
                "-".to_string(),
            ],
        ),
        other => unreachable!("no command builder for linter '{other}' — add one alongside its LintDef"),
    }
}

/// Parse `output` the way `def`'s real `nvim-lint` parser does, producing
/// quickfix entries all attributed to `path` (nvim-lint's diagnostics are
/// always about "the buffer that was linted" — there's no per-item path in
/// either tool's output).
pub fn parse_output(def: &LintDef, output: &[u8], path: &std::path::Path) -> Vec<QfItem> {
    let text = String::from_utf8_lossy(output);
    match def.name {
        "shellcheck" => parse_shellcheck_json1(&text, path),
        "ruff" => parse_ruff_json(&text, path),
        _ => Vec::new(),
    }
}

/// Port of `nvim-lint`'s `shellcheck.lua` parser: shellcheck's `--format
/// json1` wraps the diagnostic array in `{"comments": [...]}`. `level`
/// determines severity (`error`/`warning`/`info`/`style`); shellcheck's
/// `style` maps to a hint in real Neovim, which this engine's [`QfKind`]
/// doesn't have a slot for, so it becomes [`QfKind::Note`] — a cosmetic
/// difference only (still shows up, just not tagged identically).
fn parse_shellcheck_json1(text: &str, path: &std::path::Path) -> Vec<QfItem> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(decoded) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };
    let comments = decoded.get("comments").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    comments
        .iter()
        .filter_map(|item| {
            let line = item.get("line")?.as_i64()?;
            let col = item.get("column")?.as_i64()?;
            let message = item.get("message")?.as_str()?.to_string();
            let level = item.get("level").and_then(|v| v.as_str()).unwrap_or("warning");
            let kind = match level {
                "error" => QfKind::Error,
                "warning" => QfKind::Warning,
                "info" => QfKind::Info,
                _ => QfKind::Note, // shellcheck's "style"
            };
            Some(QfItem {
                path: path.to_path_buf(),
                line: (line - 1).max(0) as usize,
                col: (col - 1).max(0) as usize,
                text: message,
                kind,
            })
        })
        .collect()
}

/// Port of `nvim-lint`'s `ruff.lua` parser: `ruff check --output-format
/// json` emits a bare array of results. A handful of codes (undefined name,
/// IOError, SyntaxError) are always errors; a message starting with
/// `SyntaxError:` also is; everything else is a warning — matching the real
/// definition's severity table exactly, not just approximating it.
fn parse_ruff_json(text: &str, path: &std::path::Path) -> Vec<QfItem> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(results) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|item| {
            let location = item.get("location")?;
            let line = location.get("row")?.as_i64()?;
            let col = location.get("column")?.as_i64()?;
            let message = item.get("message")?.as_str()?.to_string();
            let code = item.get("code").and_then(|v| v.as_str()).unwrap_or("");
            let kind = if matches!(code, "F821" | "E902" | "E999") || message.starts_with("SyntaxError:") {
                QfKind::Error
            } else {
                QfKind::Warning
            };
            Some(QfItem {
                path: path.to_path_buf(),
                line: (line - 1).max(0) as usize,
                col: (col - 1).max(0) as usize,
                text: if code.is_empty() { message } else { format!("{code} {message}") },
                kind,
            })
        })
        .collect()
}

/// The buffer path a `QfItem` should carry — a real path if the buffer has
/// one, its display label otherwise (an unsaved/scratch buffer).
pub fn qf_path(buffer_path: Option<&PathBuf>, label: &str) -> PathBuf {
    buffer_path.cloned().unwrap_or_else(|| PathBuf::from(label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctrlvim_tools::lint::{RUFF, SHELLCHECK};

    #[test]
    fn builds_shellcheck_command() {
        let (bin, args) = build_command(&SHELLCHECK, "script.sh");
        assert_eq!(bin, "shellcheck");
        assert_eq!(args, vec!["--format", "json1", "-"]);
    }

    #[test]
    fn builds_ruff_command_with_the_real_buffer_path() {
        let (bin, args) = build_command(&RUFF, "src/main.py");
        assert_eq!(bin, "ruff");
        assert!(args.contains(&"src/main.py".to_string()));
        assert!(args.contains(&"--stdin-filename".to_string()));
    }

    #[test]
    fn parses_real_shellcheck_json1_output() {
        let output = r#"{"comments":[{"line":2,"column":5,"level":"error","code":2086,"message":"Double quote to prevent globbing."}]}"#;
        let items = parse_shellcheck_json1(output, std::path::Path::new("script.sh"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].line, 1);
        assert_eq!(items[0].col, 4);
        assert_eq!(items[0].kind, QfKind::Error);
        assert!(items[0].text.contains("Double quote"));
    }

    #[test]
    fn shellcheck_style_severity_becomes_a_note() {
        let output = r#"{"comments":[{"line":1,"column":1,"level":"style","message":"nit"}]}"#;
        let items = parse_shellcheck_json1(output, std::path::Path::new("s.sh"));
        assert_eq!(items[0].kind, QfKind::Note);
    }

    #[test]
    fn parses_real_ruff_json_output() {
        let output = r#"[{"code":"F401","message":"`os` imported but unused","location":{"row":1,"column":8},"end_location":{"row":1,"column":10}}]"#;
        let items = parse_ruff_json(output, std::path::Path::new("main.py"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].line, 0);
        assert_eq!(items[0].col, 7);
        assert_eq!(items[0].kind, QfKind::Warning);
        assert!(items[0].text.contains("F401"));
    }

    #[test]
    fn ruff_undefined_name_is_an_error_even_though_its_a_lint_not_a_syntax_error() {
        let output = r#"[{"code":"F821","message":"undefined name `x`","location":{"row":1,"column":1},"end_location":{"row":1,"column":2}}]"#;
        let items = parse_ruff_json(output, std::path::Path::new("main.py"));
        assert_eq!(items[0].kind, QfKind::Error);
    }

    #[test]
    fn empty_output_parses_to_no_diagnostics() {
        assert!(parse_shellcheck_json1("", std::path::Path::new("s.sh")).is_empty());
        assert!(parse_ruff_json("", std::path::Path::new("m.py")).is_empty());
    }
}
