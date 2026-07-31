//! Syntax highlighting for the built-in filetypes.
//!
//! [`ctrlvim_treesitter`] does the parsing and query work but stays
//! language-agnostic: grammars are registered by whoever embeds it. This module
//! is where the engine registers the grammars it ships with, so a frontend asks
//! for "highlight this buffer, it's Rust" and never links a grammar itself.
//!
//! Filetypes are added here one at a time as their grammar is vendored in;
//! anything not listed simply renders unstyled.

use std::sync::OnceLock;

use ctrlvim_treesitter::{HlSpan, Highlighter, Language};

/// A filetype the engine can highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filetype {
    Rust,
    Json,
    Toml,
    Lua,
    /// Block-level structure only (headings, lists, code fences, block
    /// quotes) — `tree-sitter-md` splits markdown into a block grammar and a
    /// separate inline grammar joined via injections for in-paragraph
    /// formatting (bold/italic/links), and this engine doesn't support
    /// injections yet (see `ctrlvim-treesitter`'s known gaps). A real,
    /// bounded gap: inline formatting renders unstyled, block structure
    /// doesn't.
    Markdown,
    JavaScript,
    TypeScript,
    Tsx,
    Python,
    Bash,
}

impl Filetype {
    /// Detect the filetype from a path or file name, if it's one we highlight.
    pub fn from_path(path: &str) -> Option<Filetype> {
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
        match ext.as_str() {
            "rs" => Some(Filetype::Rust),
            "json" => Some(Filetype::Json),
            "toml" => Some(Filetype::Toml),
            "lua" => Some(Filetype::Lua),
            "md" | "markdown" => Some(Filetype::Markdown),
            "js" | "jsx" | "mjs" | "cjs" => Some(Filetype::JavaScript),
            "ts" | "mts" | "cts" => Some(Filetype::TypeScript),
            "tsx" => Some(Filetype::Tsx),
            "py" | "pyi" => Some(Filetype::Python),
            "sh" | "bash" => Some(Filetype::Bash),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Filetype::Rust => "rust",
            Filetype::Json => "json",
            Filetype::Toml => "toml",
            Filetype::Lua => "lua",
            Filetype::Markdown => "markdown",
            Filetype::JavaScript => "javascript",
            Filetype::TypeScript => "typescript",
            Filetype::Tsx => "tsx",
            Filetype::Python => "python",
            Filetype::Bash => "bash",
        }
    }

    fn language(self) -> Language {
        match self {
            Filetype::Rust => tree_sitter_rust::LANGUAGE.into(),
            Filetype::Json => tree_sitter_json::LANGUAGE.into(),
            Filetype::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Filetype::Lua => tree_sitter_lua::LANGUAGE.into(),
            Filetype::Markdown => tree_sitter_md::LANGUAGE.into(),
            Filetype::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Filetype::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Filetype::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Filetype::Python => tree_sitter_python::LANGUAGE.into(),
            Filetype::Bash => tree_sitter_bash::LANGUAGE.into(),
        }
    }

    /// The grammar's `highlights.scm` — each of these is the actual upstream
    /// query the grammar crate vendors (itself either identical to or the
    /// direct source of nvim-treesitter's own query for that language, not a
    /// hand-rolled one), same as the existing Rust/JSON entries.
    ///
    /// TypeScript/TSX are the one case that needs two queries combined:
    /// `tree-sitter-typescript`'s own `HIGHLIGHTS_QUERY` only captures
    /// TypeScript-*specific* keywords/types (`interface`, `enum`, ...) — real
    /// nvim-treesitter loads it layered on top of JavaScript's query via an
    /// `; inherits: ecma` directive, since TS's grammar reuses JS's node
    /// types for everything JS already covers (`function`, `if`, `return`,
    /// ...). `Query::new` compiles one string of concatenated patterns either
    /// way, so concatenating the source text here *is* that inheritance.
    fn highlights_query(self) -> String {
        match self {
            Filetype::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
            Filetype::Json => tree_sitter_json::HIGHLIGHTS_QUERY.to_string(),
            Filetype::Toml => tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_string(),
            Filetype::Lua => tree_sitter_lua::HIGHLIGHTS_QUERY.to_string(),
            Filetype::Markdown => tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_string(),
            Filetype::JavaScript => tree_sitter_javascript::HIGHLIGHT_QUERY.to_string(),
            Filetype::TypeScript | Filetype::Tsx => {
                format!("{}\n{}", tree_sitter_javascript::HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY)
            }
            Filetype::Python => tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
            Filetype::Bash => tree_sitter_bash::HIGHLIGHT_QUERY.to_string(),
        }
    }
}

/// Highlight `source` as `filetype`, returning the spans of each line (indexed
/// by line number, empty where a line has nothing to style).
///
/// Errors (a grammar/query mismatch) degrade to "no highlighting" rather than
/// failing the render — a buffer must always be readable.
pub fn highlight(filetype: Filetype, source: &str) -> Vec<Vec<HlSpan>> {
    match highlighter(filetype) {
        Some(hl) => hl.highlight(source).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Highlight only lines `[first, last)` — what a frontend needs to draw one
/// screenful. The result is indexed from `first`.
///
/// This parses the whole buffer (a grammar needs the surrounding context) but
/// runs the highlight query over the window alone, which is what makes
/// re-highlighting on every keystroke affordable on a large file.
pub fn highlight_window(
    filetype: Filetype,
    source: &str,
    first: usize,
    last: usize,
) -> Vec<Vec<HlSpan>> {
    match highlighter(filetype) {
        Some(hl) => hl.highlight_lines_in(source, first, last).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// The process-wide [`Highlighter`] for a filetype, built on first use.
///
/// Compiling a grammar's highlight query dominates the cost of highlighting (a
/// ~30 ms one-off against ~1 ms to parse a file), so it happens once per
/// filetype rather than once per edit.
fn highlighter(filetype: Filetype) -> Option<&'static Highlighter> {
    static RUST: OnceLock<Option<Highlighter>> = OnceLock::new();
    static JSON: OnceLock<Option<Highlighter>> = OnceLock::new();
    static TOML: OnceLock<Option<Highlighter>> = OnceLock::new();
    static LUA: OnceLock<Option<Highlighter>> = OnceLock::new();
    static MARKDOWN: OnceLock<Option<Highlighter>> = OnceLock::new();
    static JAVASCRIPT: OnceLock<Option<Highlighter>> = OnceLock::new();
    static TYPESCRIPT: OnceLock<Option<Highlighter>> = OnceLock::new();
    static TSX: OnceLock<Option<Highlighter>> = OnceLock::new();
    static PYTHON: OnceLock<Option<Highlighter>> = OnceLock::new();
    static BASH: OnceLock<Option<Highlighter>> = OnceLock::new();
    let slot = match filetype {
        Filetype::Rust => &RUST,
        Filetype::Json => &JSON,
        Filetype::Toml => &TOML,
        Filetype::Lua => &LUA,
        Filetype::Markdown => &MARKDOWN,
        Filetype::JavaScript => &JAVASCRIPT,
        Filetype::TypeScript => &TYPESCRIPT,
        Filetype::Tsx => &TSX,
        Filetype::Python => &PYTHON,
        Filetype::Bash => &BASH,
    };
    slot.get_or_init(|| {
        Highlighter::new(filetype.language(), &filetype.highlights_query()).ok()
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctrlvim_treesitter::HlKind;

    fn kinds(spans: &[HlSpan]) -> Vec<HlKind> {
        spans.iter().map(|s| s.kind).collect()
    }

    #[test]
    fn detects_filetypes_we_ship_grammars_for() {
        assert_eq!(Filetype::from_path("src/main.rs"), Some(Filetype::Rust));
        assert_eq!(Filetype::from_path("Cargo.JSON"), Some(Filetype::Json));
        assert_eq!(Filetype::from_path("Cargo.toml"), Some(Filetype::Toml));
        assert_eq!(Filetype::from_path("init.lua"), Some(Filetype::Lua));
        assert_eq!(Filetype::from_path("README.md"), Some(Filetype::Markdown));
        assert_eq!(Filetype::from_path("index.js"), Some(Filetype::JavaScript));
        assert_eq!(Filetype::from_path("app.jsx"), Some(Filetype::JavaScript));
        assert_eq!(Filetype::from_path("main.ts"), Some(Filetype::TypeScript));
        assert_eq!(Filetype::from_path("App.tsx"), Some(Filetype::Tsx));
        assert_eq!(Filetype::from_path("script.py"), Some(Filetype::Python));
        assert_eq!(Filetype::from_path("install.sh"), Some(Filetype::Bash));
        assert_eq!(Filetype::from_path("notes.txt"), None);
        assert_eq!(Filetype::from_path("Makefile"), None);
    }

    /// One smoke test per newly-vendored grammar: real code in that language
    /// comes back with at least the highlight classes it obviously should
    /// have — not exhaustive coverage (the Rust tests above already prove
    /// the *engine* works precisely), just proof each grammar+query pair is
    /// wired up correctly end-to-end.
    #[test]
    fn highlights_toml() {
        let src = "[package]\nname = \"ctrlvim\"\nversion = 1\n";
        let lines = highlight(Filetype::Toml, src);
        assert!(kinds(&lines[1]).contains(&HlKind::String), "TOML string value");
    }

    #[test]
    fn highlights_lua() {
        let src = "-- comment\nlocal function f()\n  return 'x'\nend\n";
        let lines = highlight(Filetype::Lua, src);
        assert_eq!(kinds(&lines[0]), vec![HlKind::Comment]);
        assert!(kinds(&lines[1]).contains(&HlKind::Keyword), "`local`/`function` are keywords");
    }

    #[test]
    fn highlights_markdown_block_structure() {
        let src = "# Heading\n\n```rust\nfn x() {}\n```\n";
        let lines = highlight(Filetype::Markdown, src);
        assert!(!lines[0].is_empty(), "a heading line should carry some highlight");
    }

    #[test]
    fn highlights_javascript() {
        let src = "// hi\nfunction f() {\n  return 'x';\n}\n";
        let lines = highlight(Filetype::JavaScript, src);
        assert_eq!(kinds(&lines[0]), vec![HlKind::Comment]);
        assert!(kinds(&lines[1]).contains(&HlKind::Keyword), "`function` is a keyword");
    }

    #[test]
    fn highlights_typescript() {
        let src = "function f(x: number): string {\n  return x.toString();\n}\n";
        let lines = highlight(Filetype::TypeScript, src);
        assert!(kinds(&lines[0]).contains(&HlKind::Keyword), "`function` is a keyword");
    }

    #[test]
    fn highlights_tsx() {
        let src = "function App() {\n  return <div>hi</div>;\n}\n";
        let lines = highlight(Filetype::Tsx, src);
        assert!(kinds(&lines[0]).contains(&HlKind::Keyword), "`function` is a keyword");
    }

    #[test]
    fn highlights_python() {
        let src = "# comment\ndef f():\n    return 'x'\n";
        let lines = highlight(Filetype::Python, src);
        assert_eq!(kinds(&lines[0]), vec![HlKind::Comment]);
        assert!(kinds(&lines[1]).contains(&HlKind::Keyword), "`def` is a keyword");
    }

    #[test]
    fn highlights_bash() {
        let src = "# comment\nif [ -f foo ]; then\n  echo hi\nfi\n";
        let lines = highlight(Filetype::Bash, src);
        assert_eq!(kinds(&lines[0]), vec![HlKind::Comment]);
        assert!(kinds(&lines[1]).contains(&HlKind::Keyword), "`if`/`then` are keywords");
    }

    #[test]
    fn highlights_rust_keywords_strings_and_comments() {
        let src = "// hi\nfn main() {\n    let s = \"x\";\n}\n";
        let lines = highlight(Filetype::Rust, src);
        assert_eq!(kinds(&lines[0]), vec![HlKind::Comment]);
        assert!(kinds(&lines[1]).contains(&HlKind::Keyword), "`fn` is a keyword");
        assert!(kinds(&lines[1]).contains(&HlKind::Function), "`main` is a function");
        assert!(kinds(&lines[2]).contains(&HlKind::String), "`\"x\"` is a string");
    }

    #[test]
    fn spans_stay_inside_their_line() {
        let src = "fn f() {\n    let n = 42;\n}\n";
        for (i, line) in src.split('\n').enumerate() {
            let width = line.chars().count();
            for span in &highlight(Filetype::Rust, src)[i] {
                assert!(span.start < span.end, "line {i}: empty span");
                assert!(span.end <= width, "line {i}: span {span:?} past width {width}");
            }
        }
    }

    #[test]
    fn a_window_matches_the_full_highlighting_of_those_lines() {
        let src = "// one\nfn a() {}\nfn b() -> u8 { 7 }\nlet s = \"x\";\n";
        let full = highlight(Filetype::Rust, src);
        let window = highlight_window(Filetype::Rust, src, 1, 3);
        assert_eq!(window.len(), 2, "indexed from the window's first line");
        assert_eq!(window[0], full[1]);
        assert_eq!(window[1], full[2]);
    }

    #[test]
    fn a_construct_starting_above_the_window_still_highlights() {
        // The block comment opens on line 0 but the viewport starts at line 2 —
        // it must still come back as a comment, or scrolling would lose color.
        let src = "/* block\n *\n * still inside\n */\nfn after() {}\n";
        let window = highlight_window(Filetype::Rust, src, 2, 4);
        assert_eq!(kinds(&window[0]), vec![HlKind::Comment], "line 2 is inside the comment");
        assert_eq!(kinds(&window[1]), vec![HlKind::Comment], "line 3 closes it");
    }

    #[test]
    fn a_window_past_the_end_of_the_buffer_is_not_a_panic() {
        let src = "fn a() {}\n";
        assert!(highlight_window(Filetype::Rust, src, 50, 90).is_empty());
        // A window straddling the end returns just the lines that exist.
        assert_eq!(highlight_window(Filetype::Rust, src, 0, 40).len(), 2);
    }

    #[test]
    fn a_line_count_always_matches_the_source() {
        // The renderer indexes these by line, so the lengths must agree.
        for src in ["", "\n", "fn a() {}\n", "let x = 1;"] {
            assert_eq!(
                highlight(Filetype::Rust, src).len(),
                src.split('\n').count(),
                "line count mismatch for {src:?}"
            );
        }
    }
}
