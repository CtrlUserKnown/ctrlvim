//! Treesitter binding surface — the replacement for `src/ctrlvim/lua/treesitter.c`.
//!
//! The heavy lifting (parsing, queries) is the mature `tree-sitter` Rust crate;
//! this crate rebuilds the *binding surface* Neovim exposes to Lua
//! (`vim.treesitter`): parse a buffer, walk nodes, run queries, and get capture
//! ranges. Grammars are registered by name into a [`LanguageRegistry`] the
//! embedder populates (so this crate needn't hard-depend on every grammar).

use std::collections::HashMap;
use tree_sitter::{Parser, Query, QueryCursor};

/// Re-exported so embedders (`ctrlvim-lua`) can register grammars without taking a
/// direct `tree-sitter` dependency of a possibly-mismatched version.
pub use tree_sitter::Language;

/// A parsed syntax tree plus its source (needed to extract capture text).
pub struct SyntaxTree {
    tree: tree_sitter::Tree,
    source: String,
}

/// A `(row, col)` 0-based point, matching the extmark/treesitter convention.
pub type Point = (usize, usize);

/// One query capture result — what a highlighter/plugin consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// The capture name from the query (e.g. `string`, `number`, `@keyword`).
    pub name: String,
    /// The captured node's grammar kind (e.g. `string`, `pair`).
    pub kind: String,
    pub start: Point,
    pub end: Point,
    pub text: String,
}

impl SyntaxTree {
    /// The grammar kind of the root node (e.g. `document`).
    pub fn root_kind(&self) -> String {
        self.tree.root_node().kind().to_string()
    }

    /// An s-expression dump of the tree (Neovim's `:InspectTree`-style output).
    pub fn to_sexp(&self) -> String {
        self.tree.root_node().to_sexp()
    }

    /// The grammar kind of the smallest named node covering `point`.
    pub fn named_kind_at(&self, point: Point) -> Option<String> {
        let ts_point = tree_sitter::Point { row: point.0, column: point.1 };
        let root = self.tree.root_node();
        let node = root.named_descendant_for_point_range(ts_point, ts_point)?;
        Some(node.kind().to_string())
    }

    /// Run a query and return its captures in document order — the core of
    /// treesitter-based highlighting.
    pub fn query(&self, language: &Language, query_src: &str) -> Result<Vec<Capture>, String> {
        let query = Query::new(language, query_src).map_err(|e| format!("{e:?}"))?;
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let src = self.source.as_bytes();
        let mut out = Vec::new();
        for m in cursor.matches(&query, self.tree.root_node(), src) {
            for cap in m.captures {
                let node = cap.node;
                let start = node.start_position();
                let end = node.end_position();
                let text = node.utf8_text(src).unwrap_or("").to_string();
                out.push(Capture {
                    name: names[cap.index as usize].to_string(),
                    kind: node.kind().to_string(),
                    start: (start.row, start.column),
                    end: (end.row, end.column),
                    text,
                });
            }
        }
        Ok(out)
    }
}

/// Parse `source` with `language` into a [`SyntaxTree`].
pub fn parse(language: &Language, source: &str) -> Option<SyntaxTree> {
    let mut parser = Parser::new();
    parser.set_language(language).ok()?;
    let tree = parser.parse(source, None)?;
    Some(SyntaxTree { tree, source: source.to_string() })
}

/// A name→grammar registry the embedder fills in (`vim.treesitter.language.add`).
#[derive(Default)]
pub struct LanguageRegistry {
    langs: HashMap<String, Language>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        LanguageRegistry::default()
    }

    /// Register a grammar under a filetype/language name.
    pub fn add(&mut self, name: impl Into<String>, language: Language) {
        self.langs.insert(name.into(), language);
    }

    pub fn get(&self, name: &str) -> Option<&Language> {
        self.langs.get(name)
    }

    /// Parse `source` as language `name`, if registered.
    pub fn parse(&self, name: &str, source: &str) -> Option<SyntaxTree> {
        parse(self.get(name)?, source)
    }

    pub fn has(&self, name: &str) -> bool {
        self.langs.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_lang() -> Language {
        tree_sitter_json::language()
    }

    #[test]
    fn parse_json_root() {
        let tree = parse(&json_lang(), r#"{"a": 1}"#).unwrap();
        assert_eq!(tree.root_kind(), "document");
    }

    #[test]
    fn query_extracts_string_and_number() {
        let src = r#"{"name": "ada", "age": 36}"#;
        let tree = parse(&json_lang(), src).unwrap();
        let caps = tree
            .query(&json_lang(), "(string) @str (number) @num")
            .unwrap();
        // keys + values are strings; one number.
        let strings: Vec<&str> = caps.iter().filter(|c| c.name == "str").map(|c| c.text.as_str()).collect();
        assert!(strings.contains(&"\"name\""));
        assert!(strings.contains(&"\"ada\""));
        let numbers: Vec<&str> = caps.iter().filter(|c| c.name == "num").map(|c| c.text.as_str()).collect();
        assert_eq!(numbers, vec!["36"]);
    }

    #[test]
    fn named_kind_at_point() {
        let src = r#"{"k": 42}"#;
        let tree = parse(&json_lang(), src).unwrap();
        // column of the "42" number.
        let kind = tree.named_kind_at((0, 6)).unwrap();
        assert_eq!(kind, "number");
    }

    #[test]
    fn registry_parse_by_name() {
        let mut reg = LanguageRegistry::new();
        reg.add("json", json_lang());
        assert!(reg.has("json"));
        let tree = reg.parse("json", "[1, 2, 3]").unwrap();
        assert_eq!(tree.root_kind(), "document");
    }
}
