//! UI-less markdown analysis for ctrlvim's live in-editor rendering.
//!
//! This is the analysis half of the "render markdown like glow, but while you
//! edit it" feature. It is deliberately UI-agnostic: it turns raw markdown
//! source into per-line **display segments**, and the frontend
//! ([`ctrlvim-tui`]) maps each segment's [`MdKind`] to concrete colors/styles.
//! That mirrors [`ctrlvim-treesitter`], which likewise hands the embedder
//! semantic ranges rather than styles.
//!
//! # The model
//!
//! Each source line becomes an [`MdLine`] of [`Seg`]s. A segment carries both
//! the exact source slice ([`Seg::raw`]) and what to show when markup is
//! *concealed* ([`Seg::display`]). The renderer picks one:
//!
//! - On the **cursor's line** it draws `raw` (markup visible, so you can edit
//!   it) — Obsidian "live preview" behavior.
//! - On every **other line** it draws `display` (markup hidden: `**` gone,
//!   `- ` becomes `•`, `[ ]` becomes `☐`).
//!
//! A concealed marker has an empty `display`. Crucially, **one source line is
//! always one display row** — concealment only shrinks *within* a line, never
//! removes a row — so the frontend's cursor/scroll math is unaffected and no
//! source↔display column remapping is needed (the only line with a cursor is
//! rendered raw).
//!
//! This is intentionally a pragmatic, self-contained scanner rather than a
//! full CommonMark parser: it covers what live editing needs (headings,
//! emphasis, inline/fenced code, lists, task lists, quotes, links, rules).
//! Because the output type is the contract, a tree-sitter-markdown backend can
//! replace this later without the frontend changing.

/// The semantic role of a display segment. The frontend maps this to a style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdKind {
    /// Ordinary prose.
    Text,
    /// Heading text, level 1..=6.
    Heading(u8),
    /// Bold (`**x**` / `__x__`).
    Bold,
    /// Italic (`*x*` / `_x_`).
    Italic,
    /// Bold + italic (`***x***`).
    BoldItalic,
    /// Inline `` `code` `` content.
    Code,
    /// A fenced code block delimiter line (```` ``` ````).
    CodeFence,
    /// Content inside a fenced code block.
    CodeBlock,
    /// Text inside a blockquote.
    Quote,
    /// The blockquote bar (rendered from `> `).
    QuoteMarker,
    /// A list bullet or ordered-list number.
    ListMarker,
    /// A task-list checkbox; `true` = checked.
    Checkbox(bool),
    /// Visible link text.
    Link,
    /// A horizontal rule (`---`, `***`, `___`).
    Rule,
    /// Generic concealed markup (`**`, `` ` ``, `#`, link punctuation/URL, …).
    Marker,
}

/// One display segment of a line: a run of source with a single [`MdKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub kind: MdKind,
    /// The exact source slice this segment covers (shown on the cursor line).
    pub raw: String,
    /// What to show when markup is concealed (shown on non-cursor lines). An
    /// empty string means the segment is hidden entirely off the cursor line.
    pub display: String,
}

impl Seg {
    fn new(kind: MdKind, raw: impl Into<String>, display: impl Into<String>) -> Self {
        Seg { kind, raw: raw.into(), display: display.into() }
    }
    /// A segment whose display equals its source (plain, unconcealed).
    fn plain(kind: MdKind, raw: impl Into<String>) -> Self {
        let raw = raw.into();
        Seg { kind, display: raw.clone(), raw }
    }
    /// A concealed marker: source present, nothing shown when rendered.
    fn hidden(raw: impl Into<String>) -> Self {
        Seg { kind: MdKind::Marker, raw: raw.into(), display: String::new() }
    }
    /// True when this segment shows nothing off the cursor line.
    pub fn concealed(&self) -> bool {
        self.display.is_empty()
    }
}

/// One rendered line: its segments plus whether it sits inside a code block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdLine {
    pub segs: Vec<Seg>,
    /// Line lies within a fenced code block (frontend may draw a block bg).
    pub code_block: bool,
}

impl MdLine {
    /// The single [`MdKind::Rule`] segment, if this line is a horizontal rule.
    pub fn rule(&self) -> Option<&Seg> {
        match self.segs.as_slice() {
            [s] if s.kind == MdKind::Rule => Some(s),
            _ => None,
        }
    }
}

/// Analyze markdown `src` into one [`MdLine`] per source line.
///
/// Splitting is on `\n`; a trailing newline does not produce an extra empty
/// line, matching how editors count buffer lines.
pub fn analyze(src: &str) -> Vec<MdLine> {
    let mut out = Vec::new();
    let mut in_fence = false;
    let mut fence: &str = "```";

    for line in split_lines(src) {
        let trimmed = line.trim_start();
        let indent_len = line.len() - trimmed.len();

        // --- fenced code blocks (stateful across lines) --------------------
        if in_fence {
            if trimmed.starts_with(fence) && trimmed.trim_end().chars().all(|c| c == fence.as_bytes()[0] as char) {
                in_fence = false;
                out.push(MdLine { segs: vec![Seg::plain(MdKind::CodeFence, line)], code_block: true });
            } else {
                out.push(MdLine { segs: vec![Seg::plain(MdKind::CodeBlock, line)], code_block: true });
            }
            continue;
        }
        if let Some(mark) = fence_open(trimmed) {
            in_fence = true;
            fence = mark;
            out.push(MdLine { segs: vec![Seg::plain(MdKind::CodeFence, line)], code_block: true });
            continue;
        }

        // --- horizontal rule ----------------------------------------------
        if is_rule(trimmed) {
            out.push(MdLine { segs: vec![Seg::new(MdKind::Rule, line, "")], code_block: false });
            continue;
        }

        // --- ATX heading ---------------------------------------------------
        if let Some((hashes, rest, level)) = atx_heading(trimmed) {
            let mut segs = Vec::new();
            if indent_len > 0 {
                segs.push(Seg::plain(MdKind::Text, &line[..indent_len]));
            }
            segs.push(Seg::hidden(hashes));
            segs.push(Seg::plain(MdKind::Heading(level), rest));
            out.push(MdLine { segs, code_block: false });
            continue;
        }

        // --- blockquote ----------------------------------------------------
        if let Some((marker, rest)) = blockquote(line) {
            let mut segs = vec![Seg::new(MdKind::QuoteMarker, marker, "▎ ")];
            for s in inline(rest, MdKind::Quote) {
                segs.push(s);
            }
            out.push(MdLine { segs, code_block: false });
            continue;
        }

        // --- list item (unordered / ordered, optionally a task) -----------
        if let Some(list) = list_item(line) {
            let mut segs = Vec::new();
            if !list.indent.is_empty() {
                segs.push(Seg::plain(MdKind::Text, list.indent));
            }
            segs.push(Seg::new(MdKind::ListMarker, list.marker_raw, list.marker_display));
            if let Some((raw, checked)) = list.checkbox {
                let glyph = if checked { "☑ " } else { "☐ " };
                segs.push(Seg::new(MdKind::Checkbox(checked), raw, glyph));
            }
            for s in inline(list.rest, MdKind::Text) {
                segs.push(s);
            }
            out.push(MdLine { segs, code_block: false });
            continue;
        }

        // --- plain paragraph line -----------------------------------------
        let segs = inline(line, MdKind::Text);
        out.push(MdLine { segs, code_block: false });
    }

    out
}

/// Split into lines without inventing a trailing empty line, but preserving
/// genuine interior blank lines. An empty input is a single empty line.
fn split_lines(src: &str) -> Vec<&str> {
    if src.is_empty() {
        return vec![""];
    }
    let mut v: Vec<&str> = src.split('\n').collect();
    // `"a\n".split('\n')` yields ["a", ""]; drop that phantom trailing line.
    if src.ends_with('\n') {
        v.pop();
    }
    v
}

/// The fence marker (```` ``` ```` or `~~~`) if `trimmed` opens a code fence.
fn fence_open(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// A line of 3+ identical `-`, `*`, or `_` (allowing trailing spaces).
fn is_rule(trimmed: &str) -> bool {
    let t = trimmed.trim_end();
    if t.len() < 3 {
        return false;
    }
    let c = t.as_bytes()[0];
    matches!(c, b'-' | b'*' | b'_') && t.bytes().all(|b| b == c)
}

/// `## Title` → (`"## "`, `"Title"`, level). Requires a space after the hashes.
fn atx_heading(trimmed: &str) -> Option<(&str, &str, u8)> {
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &trimmed[hashes..];
    if !after.starts_with(' ') {
        return None;
    }
    let rest = after.trim_start_matches(' ');
    let marker_len = trimmed.len() - rest.len();
    Some((&trimmed[..marker_len], rest, hashes as u8))
}

/// `> quoted` → (`"> "`, `"quoted"`). Handles leading indent and nested `>`.
fn blockquote(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('>') {
        return None;
    }
    // Consume `>`s and the single optional space after the last one.
    let mut i = line.len() - trimmed.len(); // past indent
    let bytes = line.as_bytes();
    while i < bytes.len() && bytes[i] == b'>' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    Some((&line[..i], &line[i..]))
}

struct ListItem<'a> {
    indent: &'a str,
    marker_raw: &'a str,
    marker_display: String,
    checkbox: Option<(&'a str, bool)>,
    rest: &'a str,
}

/// Parse a list item: `- x`, `* x`, `+ x`, or `N. x`, with optional `[ ]`/`[x]`.
fn list_item(line: &str) -> Option<ListItem<'_>> {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let bytes = trimmed.as_bytes();

    // Marker: a bullet char + space, or digits + '.'/')' + space.
    let (marker_end, display): (usize, String) = if matches!(bytes.first(), Some(b'-' | b'*' | b'+'))
        && bytes.get(1) == Some(&b' ')
    {
        (2, "• ".to_string())
    } else {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0
            && matches!(bytes.get(digits), Some(b'.' | b')'))
            && bytes.get(digits + 1) == Some(&b' ')
        {
            let end = digits + 2;
            (end, format!("{} ", &trimmed[..digits + 1]))
        } else {
            return None;
        }
    };

    let marker_raw = &trimmed[..marker_end];
    let after = &trimmed[marker_end..];

    // Optional task-list checkbox right after the marker.
    let checkbox = task_checkbox(after);
    let rest = match &checkbox {
        Some((raw, _)) => &after[raw.len()..],
        None => after,
    };

    Some(ListItem { indent, marker_raw, marker_display: display, checkbox, rest })
}

/// `[ ] ` / `[x] ` / `[X] ` at the start → (matched-source, checked).
fn task_checkbox(s: &str) -> Option<(&str, bool)> {
    let b = s.as_bytes();
    if b.len() >= 4 && b[0] == b'[' && b[2] == b']' && b[3] == b' ' {
        match b[1] {
            b' ' => return Some((&s[..4], false)),
            b'x' | b'X' => return Some((&s[..4], true)),
            _ => {}
        }
    }
    None
}

/// Tokenize inline markup within `text`. `base` is the kind for plain runs
/// (e.g. [`MdKind::Text`] in prose, [`MdKind::Quote`] inside a blockquote).
///
/// Guarantees: concatenating every returned [`Seg::raw`] reproduces `text`
/// exactly — so the frontend can render a line's raw form by joining `raw`s.
fn inline(text: &str, base: MdKind) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let b = text.as_bytes();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !plain.is_empty() {
                out.push(Seg::plain(base, std::mem::take(&mut plain)));
            }
        };
    }

    while i < b.len() {
        let rest = &text[i..];

        // Inline code — highest precedence; content is literal.
        if b[i] == b'`' {
            if let Some(close) = rest[1..].find('`') {
                let inner = &rest[1..1 + close];
                flush!();
                out.push(Seg::hidden("`"));
                out.push(Seg::plain(MdKind::Code, inner));
                out.push(Seg::hidden("`"));
                i += 1 + close + 1;
                continue;
            }
        }

        // Bold+italic ***x***
        if let Some(inner) = fenced(rest, "***") {
            flush!();
            out.push(Seg::hidden("***"));
            out.push(Seg::plain(MdKind::BoldItalic, inner));
            out.push(Seg::hidden("***"));
            i += 6 + inner.len();
            continue;
        }
        // Bold **x** or __x__
        if let Some(inner) = fenced(rest, "**").or_else(|| fenced(rest, "__")) {
            let mark = &rest[..2];
            flush!();
            out.push(Seg::hidden(mark));
            out.push(Seg::plain(MdKind::Bold, inner));
            out.push(Seg::hidden(mark));
            i += 4 + inner.len();
            continue;
        }
        // Italic *x* or _x_
        if let Some(inner) = fenced(rest, "*").or_else(|| fenced(rest, "_")) {
            let mark = &rest[..1];
            flush!();
            out.push(Seg::hidden(mark));
            out.push(Seg::plain(MdKind::Italic, inner));
            out.push(Seg::hidden(mark));
            i += 2 + inner.len();
            continue;
        }

        // Link [text](url)
        if b[i] == b'[' {
            if let Some((label, url, consumed)) = link(rest) {
                flush!();
                out.push(Seg::hidden("["));
                out.push(Seg::plain(MdKind::Link, label));
                out.push(Seg::new(MdKind::Marker, format!("]({url})"), ""));
                i += consumed;
                continue;
            }
        }

        // Default: accumulate one UTF-8 char of plain text.
        let ch_len = utf8_len(b[i]);
        plain.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }

    flush!();
    if out.is_empty() {
        out.push(Seg::plain(base, ""));
    }
    out
}

/// If `rest` begins with `mark` and has a later closing `mark`, return the
/// non-empty inner slice (delimiters excluded). Inner must not start/end with a
/// space (so `a * b * c` isn't treated as emphasis) and must not contain the
/// marker char run again.
fn fenced<'a>(rest: &'a str, mark: &str) -> Option<&'a str> {
    if !rest.starts_with(mark) {
        return None;
    }
    let after = &rest[mark.len()..];
    let close = after.find(mark)?;
    if close == 0 {
        return None; // empty
    }
    let inner = &after[..close];
    if inner.starts_with(' ') || inner.ends_with(' ') {
        return None;
    }
    Some(inner)
}

/// Parse `[label](url)` at the start of `rest` → (label, url, bytes consumed).
fn link(rest: &str) -> Option<(&str, &str, usize)> {
    let close_br = rest.find(']')?;
    let label = &rest[1..close_br];
    let after = &rest[close_br + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let close_paren = after.find(')')?;
    let url = &after[1..close_paren];
    let consumed = close_br + 1 + close_paren + 1;
    Some((label, url, consumed))
}

/// Byte length of the UTF-8 char whose first byte is `first`.
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concatenating raw slices must always reproduce the source line — this is
    /// what lets the frontend render the cursor line verbatim.
    fn assert_raw_roundtrips(src: &str) {
        for (line, md) in src.split('\n').zip(analyze(src)) {
            let joined: String = md.segs.iter().map(|s| s.raw.as_str()).collect();
            assert_eq!(joined, line, "raw round-trip failed for {line:?}");
        }
    }

    fn kinds(md: &MdLine) -> Vec<MdKind> {
        md.segs.iter().map(|s| s.kind).collect()
    }
    fn display(md: &MdLine) -> String {
        md.segs.iter().map(|s| s.display.as_str()).collect()
    }

    #[test]
    fn heading_conceals_hashes() {
        let md = &analyze("## Title")[0];
        assert_eq!(kinds(md), vec![MdKind::Marker, MdKind::Heading(2)]);
        assert_eq!(display(md), "Title");
        assert_raw_roundtrips("## Title");
    }

    #[test]
    fn bold_italic_code_inline() {
        let md = &analyze("a **b** _c_ `d`")[0];
        assert!(md.segs.iter().any(|s| s.kind == MdKind::Bold && s.display == "b"));
        assert!(md.segs.iter().any(|s| s.kind == MdKind::Italic && s.display == "c"));
        assert!(md.segs.iter().any(|s| s.kind == MdKind::Code && s.display == "d"));
        // Markup is concealed off the cursor line.
        assert_eq!(display(md), "a b c d");
        assert_raw_roundtrips("a **b** _c_ `d`");
    }

    #[test]
    fn bold_italic_triple() {
        let md = &analyze("***wow***")[0];
        assert!(md.segs.iter().any(|s| s.kind == MdKind::BoldItalic && s.display == "wow"));
        assert_raw_roundtrips("***wow***");
    }

    #[test]
    fn unordered_list_becomes_bullet() {
        let md = &analyze("- item")[0];
        assert_eq!(md.segs[0].kind, MdKind::ListMarker);
        assert_eq!(md.segs[0].display, "• ");
        assert_eq!(display(md), "• item");
        assert_raw_roundtrips("- item");
    }

    #[test]
    fn indented_ordered_list() {
        let md = &analyze("  3. third")[0];
        assert_eq!(md.segs[0].kind, MdKind::Text); // indent preserved
        assert_eq!(md.segs[0].display, "  ");
        assert!(md.segs.iter().any(|s| s.kind == MdKind::ListMarker && s.display == "3. "));
        assert_raw_roundtrips("  3. third");
    }

    #[test]
    fn task_list_checkbox() {
        let done = &analyze("- [x] done")[0];
        assert!(done.segs.iter().any(|s| s.kind == MdKind::Checkbox(true) && s.display == "☑ "));
        let todo = &analyze("- [ ] todo")[0];
        assert!(todo.segs.iter().any(|s| s.kind == MdKind::Checkbox(false) && s.display == "☐ "));
        assert_raw_roundtrips("- [x] done");
        assert_raw_roundtrips("- [ ] todo");
    }

    #[test]
    fn link_hides_url() {
        let md = &analyze("see [docs](https://x.io) now")[0];
        assert!(md.segs.iter().any(|s| s.kind == MdKind::Link && s.display == "docs"));
        assert_eq!(display(md), "see docs now");
        assert_raw_roundtrips("see [docs](https://x.io) now");
    }

    #[test]
    fn fenced_code_block_is_literal() {
        let src = "```rust\nlet x = **not bold**;\n```";
        let lines = analyze(src);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.code_block));
        assert_eq!(lines[0].segs[0].kind, MdKind::CodeFence);
        assert_eq!(lines[1].segs[0].kind, MdKind::CodeBlock);
        // Inside a fence, markup is NOT parsed.
        assert_eq!(lines[1].segs.len(), 1);
        assert_eq!(lines[2].segs[0].kind, MdKind::CodeFence);
        assert_raw_roundtrips(src);
    }

    #[test]
    fn horizontal_rule() {
        let md = &analyze("---")[0];
        assert!(md.rule().is_some());
        assert_raw_roundtrips("---");
    }

    #[test]
    fn blockquote_bar() {
        let md = &analyze("> quoted **text**")[0];
        assert_eq!(md.segs[0].kind, MdKind::QuoteMarker);
        assert_eq!(md.segs[0].display, "▎ ");
        assert!(md.segs.iter().any(|s| s.kind == MdKind::Bold));
        assert_raw_roundtrips("> quoted **text**");
    }

    #[test]
    fn unclosed_markup_stays_plain() {
        // A lone `*` or unterminated code must not eat the rest of the line.
        let md = &analyze("2 * 3 = 6 and `oops")[0];
        assert_eq!(display(md), "2 * 3 = 6 and `oops");
        assert_raw_roundtrips("2 * 3 = 6 and `oops");
    }

    #[test]
    fn blank_and_multiline() {
        let src = "# H\n\nbody\n";
        let lines = analyze(src);
        assert_eq!(lines.len(), 3); // no phantom trailing line
        assert_eq!(lines[1].segs.len(), 1);
        assert_eq!(lines[1].segs[0].display, "");
    }

    #[test]
    fn utf8_is_not_split() {
        assert_raw_roundtrips("café — naïve **bold**");
    }
}
