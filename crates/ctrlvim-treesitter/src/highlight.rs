//! Turning a grammar's `highlights.scm` captures into per-line styled ranges.
//!
//! This is the language-agnostic half of syntax highlighting: run the query,
//! resolve the overlaps tree-sitter queries inevitably produce, and hand back
//! plain `(line, column-range, kind)` spans. Mapping [`HlKind`] to actual
//! colors is the frontend's job, so the engine stays theme-free.

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Query, QueryCursor};

/// A theme-agnostic highlight class, mapped from a capture name.
///
/// Grammars capture far more names than this (`variable.parameter`,
/// `punctuation.bracket`, …); they collapse onto the classes an editor theme
/// actually gives distinct colors to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HlKind {
    Keyword,
    Function,
    Macro,
    Type,
    Constant,
    String,
    Number,
    Comment,
    Attribute,
    Operator,
    Punctuation,
    Variable,
    Property,
}

impl HlKind {
    /// Map a capture name (`keyword.function`, `type.builtin`, …) onto a class.
    /// Unknown names return `None` and stay unstyled rather than guessing.
    pub fn from_capture(name: &str) -> Option<HlKind> {
        // Match the most specific prefix first, then the head segment.
        let head = name.split('.').next().unwrap_or(name);
        let kind = match (name, head) {
            (_, "keyword") => HlKind::Keyword,
            ("function.macro", _) => HlKind::Macro,
            (_, "function" | "method") => HlKind::Function,
            (_, "type" | "constructor") => HlKind::Type,
            ("constant.builtin", _) => HlKind::Constant,
            (_, "constant") => HlKind::Constant,
            (_, "number" | "float" | "integer" | "boolean") => HlKind::Number,
            (_, "string" | "character" | "escape") => HlKind::String,
            (_, "comment") => HlKind::Comment,
            (_, "attribute" | "annotation" | "decorator") => HlKind::Attribute,
            (_, "operator") => HlKind::Operator,
            (_, "punctuation" | "delimiter" | "bracket") => HlKind::Punctuation,
            (_, "property" | "field") => HlKind::Property,
            (_, "variable" | "parameter" | "label") => HlKind::Variable,
            _ => return None,
        };
        Some(kind)
    }
}

/// A highlighted run on one line, as **character** columns (`[start, end)`) so
/// a frontend can slice the line directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlSpan {
    pub start: usize,
    pub end: usize,
    pub kind: HlKind,
}

/// A grammar plus its compiled `highlights.scm`, ready to highlight buffers.
///
/// Compiling a real grammar's highlight query costs tens of milliseconds — far
/// more than parsing a file — so build one of these per language and keep it,
/// rather than calling [`highlight_lines`] on every edit.
pub struct Highlighter {
    language: Language,
    query: Query,
}

impl Highlighter {
    pub fn new(language: Language, query_src: &str) -> Result<Highlighter, String> {
        let query = Query::new(&language, query_src).map_err(|e| format!("{e:?}"))?;
        Ok(Highlighter { language, query })
    }

    /// Highlight `source`, returning the spans of every line, indexed by line
    /// number.
    ///
    /// Overlaps are resolved the way editors expect: an inner, narrower capture
    /// wins over the wider one containing it (the string inside a macro call is
    /// a string), and among equals the query's own order decides.
    pub fn highlight(&self, source: &str) -> Result<Vec<Vec<HlSpan>>, String> {
        highlight_with(&self.language, &self.query, source, None)
    }

    /// Highlight only lines `[first, last)` — the rows a viewport is about to
    /// draw. The whole source is still parsed (a grammar needs the full context
    /// to know what a line *is*), but the highlight query only runs over that
    /// window, which is most of the cost on a large file.
    ///
    /// The result is indexed from `first`, so element 0 is line `first`.
    pub fn highlight_lines_in(
        &self,
        source: &str,
        first: usize,
        last: usize,
    ) -> Result<Vec<Vec<HlSpan>>, String> {
        highlight_with(&self.language, &self.query, source, Some((first, last)))
    }
}

/// Compile `query_src` and highlight `source` in one call — convenient for a
/// one-shot, but see [`Highlighter`] before using it on every edit.
pub fn highlight_lines(
    language: &Language,
    query_src: &str,
    source: &str,
) -> Result<Vec<Vec<HlSpan>>, String> {
    let query = Query::new(language, query_src).map_err(|e| format!("{e:?}"))?;
    highlight_with(language, &query, source, None)
}

/// The shared implementation. `lines` restricts the work to a half-open line
/// window; `None` covers the whole source.
fn highlight_with(
    language: &Language,
    query: &Query,
    source: &str,
    lines: Option<(usize, usize)>,
) -> Result<Vec<Vec<HlSpan>>, String> {
    let tree = crate::parse(language, source).ok_or("failed to parse source")?;
    let names = query.capture_names();
    let window = lines.map(|(first, last)| byte_range_of_lines(source, first, last));

    // Collect every capture with the query pattern that produced it, so
    // precedence is decided by the query rather than by match order.
    struct Hit {
        start: usize,
        end: usize,
        pattern: usize,
        kind: HlKind,
    }
    let mut hits: Vec<Hit> = Vec::new();
    let mut cursor = QueryCursor::new();
    if let Some((start, end)) = window {
        cursor.set_byte_range(start..end);
    }
    let bytes = source.as_bytes();
    let mut matches = cursor.matches(query, tree.tree.root_node(), bytes);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let Some(kind) = HlKind::from_capture(names[cap.index as usize]) else { continue };
            let range = cap.node.byte_range();
            if range.start < range.end {
                hits.push(Hit { start: range.start, end: range.end, pattern: m.pattern_index, kind });
            }
        }
    }
    // Paint widest first so a narrower (deeper) capture overwrites the range
    // containing it; for the same range, the later query pattern wins, which is
    // how `highlights.scm` files are written (general rules first, refinements
    // after) and how Neovim resolves the same conflict.
    hits.sort_by_key(|h| (std::cmp::Reverse(h.end - h.start), h.pattern));

    let mut canvas: Vec<Option<HlKind>> = vec![None; source.len()];
    for hit in hits {
        for slot in &mut canvas[hit.start..hit.end.min(source.len())] {
            *slot = Some(hit.kind);
        }
    }

    Ok(spans_per_line(source, &canvas, lines))
}

/// The byte range covering lines `[first, last)` of `source`.
fn byte_range_of_lines(source: &str, first: usize, last: usize) -> (usize, usize) {
    let mut start = source.len();
    let mut end = source.len();
    let mut offset = 0usize;
    for (i, line) in source.split('\n').enumerate() {
        if i == first {
            start = offset;
        }
        offset += line.len() + 1; // + the '\n' `split` consumed
        if i + 1 == last {
            end = offset.min(source.len());
            break;
        }
    }
    (start.min(end), end)
}

/// Compress the byte canvas into per-line runs, converting byte offsets to
/// character columns as it goes. `lines` limits the output to a window, which
/// is then indexed from its first line.
fn spans_per_line(
    source: &str,
    canvas: &[Option<HlKind>],
    lines: Option<(usize, usize)>,
) -> Vec<Vec<HlSpan>> {
    let mut out = Vec::new();
    let mut line_start = 0usize; // byte offset of the current line
    for (i, line) in source.split('\n').enumerate() {
        if let Some((first, last)) = lines {
            if i < first || i >= last {
                line_start += line.len() + 1;
                continue;
            }
        }
        let mut spans: Vec<HlSpan> = Vec::new();
        let mut open: Option<(usize, HlKind)> = None; // (start column, kind)
        let mut col = 0usize;
        for (offset, _) in line.char_indices() {
            let kind = canvas.get(line_start + offset).copied().flatten();
            match open {
                // The run continues.
                Some((_, prev)) if Some(prev) == kind => {}
                // It ended (or changed class): close it and maybe open the next.
                Some((start, prev)) => {
                    spans.push(HlSpan { start, end: col, kind: prev });
                    open = kind.map(|k| (col, k));
                }
                None => open = kind.map(|k| (col, k)),
            }
            col += 1;
        }
        if let Some((start, kind)) = open {
            spans.push(HlSpan { start, end: col, kind });
        }
        out.push(spans);
        line_start += line.len() + 1; // + the '\n' that `split` consumed
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json() -> Language {
        tree_sitter_json::LANGUAGE.into()
    }

    const JSON_HL: &str = r#"
        (string) @string
        (number) @number
        (pair key: (string) @property)
        ["{" "}" "[" "]"] @punctuation.bracket
        [":" ","] @punctuation.delimiter
    "#;

    #[test]
    fn capture_names_map_onto_classes() {
        assert_eq!(HlKind::from_capture("keyword"), Some(HlKind::Keyword));
        assert_eq!(HlKind::from_capture("keyword.function"), Some(HlKind::Keyword));
        assert_eq!(HlKind::from_capture("function.macro"), Some(HlKind::Macro));
        assert_eq!(HlKind::from_capture("function.method"), Some(HlKind::Function));
        assert_eq!(HlKind::from_capture("type.builtin"), Some(HlKind::Type));
        assert_eq!(HlKind::from_capture("punctuation.bracket"), Some(HlKind::Punctuation));
        assert_eq!(HlKind::from_capture("spaceship.warp"), None);
    }

    #[test]
    fn highlights_land_on_the_right_columns() {
        let src = "{\n  \"age\": 36\n}";
        let lines = highlight_lines(&json(), JSON_HL, src).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], vec![HlSpan { start: 0, end: 1, kind: HlKind::Punctuation }]);
        // `"age"` is a pair key (property, not plain string), then `:`, then 36.
        assert_eq!(
            lines[1],
            vec![
                HlSpan { start: 2, end: 7, kind: HlKind::Property },
                HlSpan { start: 7, end: 8, kind: HlKind::Punctuation },
                HlSpan { start: 9, end: 11, kind: HlKind::Number },
            ]
        );
    }

    #[test]
    fn columns_are_characters_not_bytes() {
        // The emoji is 4 bytes; the number after it must still start at the
        // char column a renderer would slice at.
        let src = "{\"🌍\": 1}";
        let lines = highlight_lines(&json(), JSON_HL, src).unwrap();
        // `{`, `"`, 🌍, `"`, `:`, ` `, `1` — the number is char column 6, though
        // it sits at byte offset 9.
        let number = lines[0].iter().find(|s| s.kind == HlKind::Number).unwrap();
        assert_eq!(number.start, 6, "char column, not byte offset");
        assert_eq!(number.end, 7);
    }

    #[test]
    fn inner_captures_win_over_the_ranges_containing_them() {
        // The key string sits inside the pair; the narrower capture decides.
        let src = r#"{"k": "v"}"#;
        let lines = highlight_lines(&json(), JSON_HL, src).unwrap();
        let kinds: Vec<HlKind> = lines[0].iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&HlKind::Property), "key is a property");
        assert!(kinds.contains(&HlKind::String), "value stays a string");
    }

    #[test]
    fn empty_and_unparseable_input_are_not_a_panic() {
        assert_eq!(highlight_lines(&json(), JSON_HL, "").unwrap(), vec![Vec::new()]);
        // Broken source still parses (with error nodes) and highlights what it can.
        let lines = highlight_lines(&json(), JSON_HL, "{\"a\": ").unwrap();
        assert!(!lines.is_empty());
        // A bad query is reported, not swallowed.
        assert!(highlight_lines(&json(), "(nonsense) @x", "{}").is_err());
    }
}
