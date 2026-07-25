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
}

impl Filetype {
    /// Detect the filetype from a path or file name, if it's one we highlight.
    pub fn from_path(path: &str) -> Option<Filetype> {
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
        match ext.as_str() {
            "rs" => Some(Filetype::Rust),
            "json" => Some(Filetype::Json),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Filetype::Rust => "rust",
            Filetype::Json => "json",
        }
    }

    fn language(self) -> Language {
        match self {
            Filetype::Rust => tree_sitter_rust::LANGUAGE.into(),
            Filetype::Json => tree_sitter_json::LANGUAGE.into(),
        }
    }

    /// The grammar's `highlights.scm`.
    fn highlights_query(self) -> &'static str {
        match self {
            Filetype::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
            Filetype::Json => tree_sitter_json::HIGHLIGHTS_QUERY,
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
    let slot = match filetype {
        Filetype::Rust => &RUST,
        Filetype::Json => &JSON,
    };
    slot.get_or_init(|| {
        Highlighter::new(filetype.language(), filetype.highlights_query()).ok()
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
        assert_eq!(Filetype::from_path("notes.txt"), None);
        assert_eq!(Filetype::from_path("Makefile"), None);
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
