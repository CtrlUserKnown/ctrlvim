//! Tags — the model half of `tag.c`.
//!
//! A tags file (as `ctags -R .` writes) maps an identifier to the file and
//! position that defines it. `Ctrl-]` jumps there, `Ctrl-T` comes back, and the
//! stack of where-you-came-from is the tagstack.
//!
//! Parsing, searching, and the stack live here; *reading* the tags file is the
//! host's job, like every other filesystem touch (see [`crate::ex::ExEffect`]).

/// Where in a file a tag is defined.
///
/// ctags emits either a line number or a search pattern; patterns survive edits
/// above the definition, which is why they're the common case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagAddress {
    /// 0-based line.
    Line(usize),
    /// The text of a `/^…$/` search pattern, anchors and escapes removed.
    Pattern(String),
}

/// One entry from a tags file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// Path as written in the tags file — relative to the file's directory.
    pub path: String,
    pub address: TagAddress,
    /// ctags "kind" (`f` function, `s` struct, …) when the extended format
    /// provides it.
    pub kind: Option<char>,
}

/// A parsed tags file, sorted by name so lookups can binary-search.
#[derive(Debug, Clone, Default)]
pub struct TagTable {
    tags: Vec<Tag>,
}

impl TagTable {
    pub fn new() -> Self {
        TagTable::default()
    }

    /// Parse a tags file's contents. Unparseable lines are skipped rather than
    /// failing the load — a tags file is generated output, and one odd line
    /// shouldn't cost the user every other tag.
    pub fn parse(text: &str) -> TagTable {
        let mut tags: Vec<Tag> = text.lines().filter_map(parse_line).collect();
        // ctags writes sorted output, but `--sort=no` exists and concatenated
        // files aren't sorted, so don't trust it.
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        TagTable { tags }
    }

    pub fn len(&self) -> usize {
        self.tags.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Every tag with exactly this name, in file order.
    pub fn find(&self, name: &str) -> &[Tag] {
        let start = self.tags.partition_point(|t| t.name.as_str() < name);
        let end = self.tags.partition_point(|t| t.name.as_str() <= name);
        &self.tags[start..end]
    }

    /// Names starting with `prefix`, for completion (`:tag foo<Tab>`).
    pub fn with_prefix<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a Tag> + 'a {
        let start = self.tags.partition_point(|t| t.name.as_str() < prefix);
        self.tags[start..].iter().take_while(move |t| t.name.starts_with(prefix))
    }
}

/// Parse one `name<TAB>file<TAB>address[;" extensions]` line.
fn parse_line(line: &str) -> Option<Tag> {
    // `!_TAG_FILE_FORMAT` and friends are metadata, not tags.
    if line.starts_with('!') || line.trim().is_empty() {
        return None;
    }
    let mut fields = line.splitn(3, '\t');
    let name = fields.next()?.to_string();
    let path = fields.next()?.to_string();
    let rest = fields.next()?;
    if name.is_empty() || path.is_empty() {
        return None;
    }

    // The address ends at `;"`, after which come extension fields.
    let (address_src, extensions) = match rest.find(";\"") {
        Some(i) => (&rest[..i], &rest[i + 2..]),
        None => (rest, ""),
    };
    let address = parse_address(address_src.trim())?;
    // Extended fields are tab-separated `key:value`, plus a bare kind letter.
    let mut kind = None;
    for field in extensions.split('\t').map(str::trim).filter(|f| !f.is_empty()) {
        if let Some(k) = field.strip_prefix("kind:") {
            kind = k.chars().next();
        } else if field.len() == 1 {
            kind = field.chars().next();
        }
    }
    Some(Tag { name, path, address, kind })
}

/// Parse the address field: a line number, or a `/^text$/` search pattern.
fn parse_address(src: &str) -> Option<TagAddress> {
    if let Ok(n) = src.parse::<usize>() {
        // Tags files are 1-based; the engine is 0-based throughout.
        return Some(TagAddress::Line(n.saturating_sub(1)));
    }
    let inner = src.strip_prefix('/')?.strip_suffix('/')?;
    let inner = inner.strip_prefix('^').unwrap_or(inner);
    let inner = inner.strip_suffix('$').unwrap_or(inner);
    // ctags escapes `/` and `\` inside the pattern.
    let mut text = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(escaped) => text.push(escaped),
                None => text.push('\\'),
            }
        } else {
            text.push(c);
        }
    }
    Some(TagAddress::Pattern(text))
}

/// Resolve an address against a file's lines, returning a 0-based line.
///
/// A pattern is matched exactly first, then by prefix, then anywhere — the
/// definition may have been reindented or lightly edited since the tags file
/// was written, and landing on the right line beats not jumping at all.
pub fn resolve_address(address: &TagAddress, lines: &[String]) -> Option<usize> {
    match address {
        TagAddress::Line(n) => Some((*n).min(lines.len().saturating_sub(1))),
        TagAddress::Pattern(pattern) => {
            let pattern = pattern.trim();
            if pattern.is_empty() {
                return Some(0);
            }
            lines
                .iter()
                .position(|l| l == pattern)
                .or_else(|| lines.iter().position(|l| l.trim_start() == pattern.trim_start()))
                .or_else(|| lines.iter().position(|l| l.contains(pattern)))
        }
    }
}

/// One "where I jumped from" record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagStackEntry {
    /// The tag that was jumped to.
    pub name: String,
    /// The file the jump started in (empty for an unnamed buffer).
    pub path: String,
    pub line: usize,
    pub col: usize,
}

/// The tagstack: `Ctrl-]` pushes, `Ctrl-T` pops.
#[derive(Debug, Clone, Default)]
pub struct TagStack {
    entries: Vec<TagStackEntry>,
}

impl TagStack {
    pub fn new() -> Self {
        TagStack::default()
    }

    pub fn push(&mut self, entry: TagStackEntry) {
        self.entries.push(entry);
    }

    pub fn pop(&mut self) -> Option<TagStackEntry> {
        self.entries.pop()
    }

    pub fn entries(&self) -> &[TagStackEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The current `Ctrl-]` match list, so `:tnext`/`:tprev` can walk the
/// definitions of an overloaded name.
#[derive(Debug, Clone, Default)]
pub struct TagMatches {
    pub name: String,
    pub matches: Vec<Tag>,
    pub current: usize,
}

impl TagMatches {
    /// `:tnext` / `:tprev` — move by `by` matches.
    ///
    /// Returns `None` when the move isn't possible because the list is already
    /// at that end, so the caller can report Vim's "no more matching tags"
    /// rather than silently re-jumping to the same definition.
    pub fn advance(&mut self, by: isize) -> Option<&Tag> {
        if self.matches.is_empty() || by == 0 {
            return None;
        }
        let last = self.matches.len() as isize - 1;
        let target = (self.current as isize + by).clamp(0, last);
        if target == self.current as isize {
            return None;
        }
        self.current = target as usize;
        self.matches.get(self.current)
    }

    /// `:tfirst` / `:tlast` — jump to an end of the list, which always works.
    pub fn goto_end(&mut self, last: bool) -> Option<&Tag> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = if last { self.matches.len() - 1 } else { 0 };
        self.matches.get(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic `ctags -R` fixture, extended format.
    const TAGS: &str = "\
!_TAG_FILE_FORMAT\t2\t/extended format/
!_TAG_FILE_SORTED\t1\t/0=unsorted, 1=sorted/
Editor\tsrc/editor.rs\t/^pub struct Editor {$/;\"\ts
apply_operator\tsrc/operator.rs\t/^pub fn apply_operator($/;\"\tf
main\tsrc/main.rs\t42;\"\tf
main\tsrc/bin/demo.rs\t/^fn main() {$/;\"\tf
";

    #[test]
    fn parses_the_extended_ctags_format() {
        let table = TagTable::parse(TAGS);
        assert_eq!(table.len(), 4, "metadata lines are not tags");

        let editor = &table.find("Editor")[0];
        assert_eq!(editor.path, "src/editor.rs");
        assert_eq!(editor.address, TagAddress::Pattern("pub struct Editor {".into()));
        assert_eq!(editor.kind, Some('s'));
    }

    #[test]
    fn parses_line_number_addresses_as_zero_based() {
        let table = TagTable::parse(TAGS);
        let main = table.find("main");
        assert_eq!(main.len(), 2, "a name can have several definitions");
        assert!(main.iter().any(|t| t.address == TagAddress::Line(41)), "42 → 0-based 41");
    }

    #[test]
    fn find_returns_nothing_for_an_unknown_name() {
        let table = TagTable::parse(TAGS);
        assert!(table.find("nonexistent").is_empty());
        assert!(table.find("").is_empty());
        // Prefix search is a different question from exact match.
        assert!(table.find("mai").is_empty());
        assert_eq!(table.with_prefix("mai").count(), 2);
    }

    #[test]
    fn unsorted_and_malformed_input_still_loads() {
        let text = "zeta\tz.rs\t1\nalpha\ta.rs\t2\ngarbage line with no tabs\n\n";
        let table = TagTable::parse(text);
        assert_eq!(table.len(), 2, "the junk line is skipped");
        // Sorted on load, so binary search is valid even for `--sort=no` files.
        assert_eq!(table.find("alpha")[0].path, "a.rs");
        assert_eq!(table.find("zeta")[0].path, "z.rs");
    }

    #[test]
    fn unescapes_slashes_in_patterns() {
        let table = TagTable::parse("re\tx.rs\t/^let re = \\/x\\/;$/;\"\tv\n");
        assert_eq!(table.find("re")[0].address, TagAddress::Pattern("let re = /x/;".into()));
    }

    #[test]
    fn resolves_a_pattern_to_its_line() {
        let lines: Vec<String> = "use x;\n\npub struct Editor {\n    field: u8,\n}"
            .lines()
            .map(str::to_string)
            .collect();
        let addr = TagAddress::Pattern("pub struct Editor {".into());
        assert_eq!(resolve_address(&addr, &lines), Some(2));
    }

    #[test]
    fn resolves_a_pattern_that_moved_or_was_reindented() {
        let lines: Vec<String> =
            vec!["mod a {".into(), "    pub fn helper() {}".into(), "}".into()];
        // The tags file recorded it unindented; it matches on the trimmed form.
        let addr = TagAddress::Pattern("pub fn helper() {}".into());
        assert_eq!(resolve_address(&addr, &lines), Some(1));
        // A pattern that isn't there at all resolves to nothing, not line 0.
        assert_eq!(resolve_address(&TagAddress::Pattern("gone".into()), &lines), None);
    }

    #[test]
    fn a_line_address_past_the_end_clamps() {
        let lines: Vec<String> = vec!["one".into(), "two".into()];
        assert_eq!(resolve_address(&TagAddress::Line(99), &lines), Some(1));
    }

    #[test]
    fn the_tagstack_is_last_in_first_out() {
        let mut stack = TagStack::new();
        assert!(stack.pop().is_none());
        for (i, name) in ["a", "b"].iter().enumerate() {
            stack.push(TagStackEntry {
                name: name.to_string(),
                path: format!("f{i}.rs"),
                line: i,
                col: 0,
            });
        }
        assert_eq!(stack.entries().len(), 2);
        assert_eq!(stack.pop().unwrap().name, "b");
        assert_eq!(stack.pop().unwrap().name, "a");
        assert!(stack.is_empty());
    }

    #[test]
    fn tag_matches_walk_without_wrapping() {
        let table = TagTable::parse(TAGS);
        let mut matches = TagMatches {
            name: "main".into(),
            matches: table.find("main").to_vec(),
            current: 0,
        };
        assert_eq!(matches.advance(1).unwrap().name, "main");
        assert_eq!(matches.current, 1);
        matches.advance(5);
        assert_eq!(matches.current, 1, "clamped at the last match");
        matches.advance(-9);
        assert_eq!(matches.current, 0);
    }
}
