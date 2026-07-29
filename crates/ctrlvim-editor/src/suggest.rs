//! Inline AI suggestions: the "ghost text" a completion model proposes ahead of
//! the cursor, and the state machine deciding when it is valid.
//!
//! This is the engine half of the feature. It owns *what* is being suggested,
//! *where* it applies, when it goes stale, and what accepting part of it does to
//! the buffer. It knows nothing about models, threads, or HTTP: running the
//! completion is host work, requested through [`SuggestRequest`] and delivered
//! back through [`InlineSuggest::fulfill`] — the same engine-owns-the-data,
//! host-owns-the-I/O split the quickfix list and the tag table already use.
//!
//! # Staleness
//!
//! A completion takes far longer to produce than a keystroke takes to type, so
//! nearly every reply arrives against a buffer that has already moved on. Every
//! context change bumps [`InlineSuggest::seq`], a request carries the seq it was
//! issued at, and a reply is only installed if that seq is still the current
//! one. Without this, ghost text from three keystrokes ago would appear at the
//! new cursor position and read as gibberish.

use ctrlvim_types::Position;

/// How much of a suggestion an accept command takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accept {
    /// The whole suggestion, however many lines it spans (`<Tab>`).
    All,
    /// Up to and including the end of the first line (`<C-j>`).
    Line,
    /// The next word — leading whitespace plus one run of word characters, or
    /// one run of punctuation (`<C-l>`).
    Word,
}

/// A proposal from the completion model, anchored to where it was requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Buffer position the ghost text starts at: the cursor when the request
    /// was issued. Rendering draws `text` from here; accepting inserts there.
    pub anchor: Position,
    /// The proposed text. May span several lines, `\n`-separated, and never
    /// ends with a newline (a trailing one is trimmed on construction, since a
    /// suggestion that is only a line break has nothing to show).
    pub text: String,
}

impl Suggestion {
    /// Build a suggestion, normalizing the model's text. Returns `None` for
    /// anything with nothing to display, so callers never have to special-case
    /// an empty ghost.
    pub fn new(anchor: Position, text: impl Into<String>) -> Option<Self> {
        let text = text.into();
        // `\r` would render as a stray glyph in the middle of a line.
        let text = text.replace('\r', "");
        let text = text.trim_end_matches('\n').to_string();
        if text.is_empty() {
            return None;
        }
        Some(Suggestion { anchor, text })
    }

    /// The part of the suggestion drawn on the anchor's own row — i.e. the
    /// inline ghost text, as opposed to the extra rows below it.
    pub fn head(&self) -> &str {
        match self.text.split_once('\n') {
            Some((first, _)) => first,
            None => &self.text,
        }
    }

    /// The rows drawn *below* the anchor's line, empty for a single-line
    /// suggestion.
    pub fn tail(&self) -> Vec<&str> {
        match self.text.split_once('\n') {
            Some((_, rest)) => rest.split('\n').collect(),
            None => Vec::new(),
        }
    }

    /// Whether the suggestion spills past the anchor's line.
    pub fn is_multiline(&self) -> bool {
        self.text.contains('\n')
    }

    /// The text an [`Accept`] command inserts. Always a prefix of `text`, so
    /// what remains after accepting is simply the rest.
    pub fn portion(&self, what: Accept) -> &str {
        match what {
            Accept::All => &self.text,
            Accept::Line => match self.text.find('\n') {
                // Include the newline: accepting "a line" of a multi-line
                // suggestion should leave the cursor on the next line, the way
                // it would if the user had typed it.
                Some(i) => &self.text[..=i],
                None => &self.text,
            },
            Accept::Word => &self.text[..word_end(&self.text)],
        }
    }
}

/// Byte offset one past the first "word" of `s`, for `<C-l>`-style partial
/// accepts.
///
/// Leading whitespace belongs to the word that follows it (accepting a word of
/// `" foo"` should give you `" foo"`, not strand a space); a run of
/// non-alphanumerics is a word of its own, so `"(x)"` accepts as `"("` rather
/// than swallowing the identifier. A newline ends the word, since crossing a
/// line boundary is [`Accept::Line`]'s job.
fn word_end(s: &str) -> usize {
    let mut it = s.char_indices().peekable();
    // Leading blanks (but never a line break).
    while let Some(&(_, c)) = it.peek() {
        if c == '\n' || !c.is_whitespace() {
            break;
        }
        it.next();
    }
    let Some(&(start, first)) = it.peek() else { return s.len() };
    if first == '\n' {
        // Nothing but blanks before the line break: take the blanks.
        return start;
    }
    let wordish = first.is_alphanumeric() || first == '_';
    for (i, c) in it {
        if c == '\n' {
            return i;
        }
        let this = c.is_alphanumeric() || c == '_';
        if this != wordish {
            return i;
        }
        if !wordish && !c.is_whitespace() && i > start {
            // Punctuation runs stop after one character, so accepting a word of
            // `"));"` gives `")"` and not the whole trailer.
            return i;
        }
    }
    s.len()
}

/// Everything the host needs to produce one completion, plus the seq that says
/// which edit state it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestRequest {
    /// Edit-state token; hand it back to [`InlineSuggest::fulfill`] so a reply
    /// for a buffer that has since changed is dropped instead of shown.
    pub seq: u64,
    /// Cursor position the completion continues from.
    pub anchor: Position,
    /// Buffer text before the cursor, truncated to the configured window.
    pub prefix: String,
    /// Buffer text after the cursor, truncated to the configured window.
    pub suffix: String,
    /// The buffer's name, when it has one — the model does better with the
    /// filename in scope, and it's how the host picks a language hint.
    pub filename: Option<String>,
}

/// How much buffer context a request carries. Bigger windows give better
/// completions and cost linearly more prefill time, which on CPU inference is
/// the dominant cost — hence the deliberately modest defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextWindow {
    /// Lines before the cursor to include.
    pub before: usize,
    /// Lines after the cursor to include.
    pub after: usize,
}

impl Default for ContextWindow {
    fn default() -> Self {
        ContextWindow { before: 20, after: 6 }
    }
}

/// The inline-suggestion state machine for a session.
#[derive(Debug, Clone)]
pub struct InlineSuggest {
    /// Whether suggestions are being offered at all (`:AIToggle`).
    pub enabled: bool,
    /// How much surrounding text a request carries.
    pub context: ContextWindow,
    /// The ghost text currently on screen, if any.
    current: Option<Suggestion>,
    /// Bumped by every context change; requests and replies are tagged with it.
    seq: u64,
    /// Set when the context changed and no request has been issued for it yet.
    dirty: bool,
    /// Seq of the request the host is currently working on, if any. Keeps one
    /// completion in flight at a time — the model is the bottleneck, and
    /// queueing requests behind a stale one only adds latency.
    inflight: Option<u64>,
}

impl Default for InlineSuggest {
    fn default() -> Self {
        InlineSuggest {
            // Off until a host opts in: the engine has no model, and a core
            // whose default behaviour depends on a 2B-parameter download is not
            // a core anyone can test.
            enabled: false,
            context: ContextWindow::default(),
            current: None,
            seq: 0,
            dirty: false,
            inflight: None,
        }
    }
}

impl InlineSuggest {
    /// The ghost text to render, if any.
    pub fn current(&self) -> Option<&Suggestion> {
        self.current.as_ref()
    }

    /// Whether a request is out with the host right now (drives the "thinking"
    /// indicator).
    pub fn is_pending(&self) -> bool {
        self.inflight.is_some()
    }

    /// The context changed: drop the ghost text and mark a new completion as
    /// worth asking for. Any reply still in flight is now stale.
    pub fn invalidate(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        self.current = None;
        self.inflight = None;
        self.dirty = self.enabled;
    }

    /// Leaving Insert mode (or turning the feature off): drop the ghost text
    /// without asking for a replacement.
    pub fn disarm(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        self.current = None;
        self.inflight = None;
        self.dirty = false;
    }

    /// Dismiss the visible suggestion but leave the feature armed — `<C-e>`.
    /// Deliberately does *not* set `dirty`: asking again for the same context
    /// the user just rejected would put the same ghost straight back.
    pub fn dismiss(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        self.current = None;
        self.inflight = None;
        self.dirty = false;
    }

    /// Ask for a completion now even though nothing changed (`:AISuggest`).
    pub fn arm(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        self.current = None;
        self.inflight = None;
        self.dirty = true;
    }

    /// Whether a request should be issued: something changed, nothing is in
    /// flight, and nothing is currently displayed.
    pub fn wants_request(&self) -> bool {
        self.enabled && self.dirty && self.inflight.is_none() && self.current.is_none()
    }

    /// Record that a request for the current state has gone to the host.
    fn issue(&mut self) -> u64 {
        self.dirty = false;
        self.inflight = Some(self.seq);
        self.seq
    }

    /// Install a reply. Returns false — and changes nothing — if the buffer has
    /// moved on since the request was issued.
    pub fn fulfill(&mut self, seq: u64, anchor: Position, text: &str) -> bool {
        if self.inflight != Some(seq) || self.seq != seq {
            return false;
        }
        self.inflight = None;
        self.current = Suggestion::new(anchor, text);
        self.current.is_some()
    }

    /// Record that a request produced nothing (an error, or an empty
    /// completion). Leaves the state clean so the next edit can try again.
    pub fn fail(&mut self, seq: u64) {
        if self.inflight == Some(seq) {
            self.inflight = None;
        }
    }

    /// Take part of the suggestion for insertion, advancing or clearing what
    /// remains. Returns the text to insert.
    fn take(&mut self, what: Accept) -> Option<String> {
        let s = self.current.as_ref()?;
        let portion = s.portion(what).to_string();
        if portion.is_empty() {
            return None;
        }
        if portion.len() >= s.text.len() {
            // Consumed the lot.
            self.seq = self.seq.wrapping_add(1);
            self.current = None;
            self.dirty = false;
        } else {
            // Keep the rest, re-anchored past what was just inserted. The seq
            // moves too: the buffer changed, so any reply still out is stale.
            let rest = s.text[portion.len()..].to_string();
            let anchor = advance(s.anchor, &portion);
            self.seq = self.seq.wrapping_add(1);
            self.inflight = None;
            self.dirty = false;
            self.current = Suggestion::new(anchor, rest);
        }
        Some(portion)
    }
}

/// The position reached by inserting `text` at `from`. Columns are byte
/// offsets, matching [`Position`].
fn advance(from: Position, text: &str) -> Position {
    match text.rsplit_once('\n') {
        Some((before, last)) => {
            Position::new(from.line + before.matches('\n').count() + 1, last.len())
        }
        None => Position::new(from.line, from.col + text.len()),
    }
}

/// Slice a suggestion request's prefix/suffix out of a buffer's lines.
///
/// `cursor.col` is a byte column, as everywhere else in the engine, and is
/// clamped to the line so a cursor sitting one past the end (Insert mode's
/// resting place) yields the whole line as prefix rather than panicking.
pub fn split_context(
    lines: &[String],
    cursor: Position,
    window: ContextWindow,
) -> (String, String) {
    if lines.is_empty() {
        return (String::new(), String::new());
    }
    let line = cursor.line.min(lines.len() - 1);
    let text = &lines[line];
    let col = clamp_boundary(text, cursor.col);

    let first = line.saturating_sub(window.before);
    let mut prefix = String::new();
    for l in &lines[first..line] {
        prefix.push_str(l);
        prefix.push('\n');
    }
    prefix.push_str(&text[..col]);

    let last = (line + 1 + window.after).min(lines.len());
    let mut suffix = String::from(&text[col..]);
    for l in &lines[line + 1..last] {
        suffix.push('\n');
        suffix.push_str(l);
    }
    (prefix, suffix)
}

/// Clamp a byte column to the nearest char boundary at or below it, so slicing
/// a line at the cursor can't split a multi-byte character.
fn clamp_boundary(s: &str, col: usize) -> usize {
    let mut col = col.min(s.len());
    while col > 0 && !s.is_char_boundary(col) {
        col -= 1;
    }
    col
}

/// Build a request for the given buffer state, or `None` if one isn't wanted.
/// Split out from [`InlineSuggest`] so it can be exercised without a session.
pub fn build_request(
    state: &mut InlineSuggest,
    lines: &[String],
    cursor: Position,
    filename: Option<String>,
) -> Option<SuggestRequest> {
    if !state.wants_request() {
        return None;
    }
    let (prefix, suffix) = split_context(lines, cursor, state.context);
    let seq = state.issue();
    Some(SuggestRequest { seq, anchor: cursor, prefix, suffix, filename })
}

/// Accept part of a suggestion: returns the text to insert, having advanced the
/// state machine. Callers do the actual buffer edit.
pub fn accept(state: &mut InlineSuggest, what: Accept) -> Option<String> {
    state.take(what)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sug(text: &str) -> Suggestion {
        Suggestion::new(Position::new(0, 0), text).expect("non-empty")
    }

    #[test]
    fn an_empty_or_newline_only_suggestion_is_nothing_to_show() {
        assert!(Suggestion::new(Position::default(), "").is_none());
        assert!(Suggestion::new(Position::default(), "\n\n").is_none());
        assert!(Suggestion::new(Position::default(), "x").is_some());
    }

    #[test]
    fn head_and_tail_split_at_the_anchor_line() {
        let s = sug("foo(bar)\n    baz\n}");
        assert_eq!(s.head(), "foo(bar)");
        assert_eq!(s.tail(), vec!["    baz", "}"]);
        assert!(s.is_multiline());

        let one = sug("foo");
        assert_eq!(one.head(), "foo");
        assert!(one.tail().is_empty());
        assert!(!one.is_multiline());
    }

    #[test]
    fn accepting_a_word_keeps_its_leading_blanks() {
        assert_eq!(sug(" world").portion(Accept::Word), " world");
        assert_eq!(sug("world foo").portion(Accept::Word), "world");
        assert_eq!(sug("snake_case()").portion(Accept::Word), "snake_case");
    }

    #[test]
    fn a_word_accept_takes_one_punctuation_character_at_a_time() {
        // The regression this guards: `word_end` used to run to the end of a
        // punctuation run, so accepting one "word" of `"));"` took the whole
        // trailer and left nothing to review.
        assert_eq!(sug("));").portion(Accept::Word), ")");
        assert_eq!(sug("(x)").portion(Accept::Word), "(");
    }

    #[test]
    fn a_word_accept_stops_at_a_line_break() {
        assert_eq!(sug("foo\nbar").portion(Accept::Word), "foo");
        assert_eq!(sug("  \nbar").portion(Accept::Word), "  ");
    }

    #[test]
    fn accepting_a_line_includes_its_break_so_the_cursor_lands_below() {
        assert_eq!(sug("one\ntwo\nthree").portion(Accept::Line), "one\n");
        assert_eq!(sug("only").portion(Accept::Line), "only");
    }

    #[test]
    fn a_partial_accept_re_anchors_the_rest() {
        let mut st = InlineSuggest { enabled: true, ..Default::default() };
        st.arm();
        let seq = st.issue();
        assert!(st.fulfill(seq, Position::new(3, 4), "foo bar"));

        assert_eq!(accept(&mut st, Accept::Word).as_deref(), Some("foo"));
        let rest = st.current().expect("the rest is still offered");
        assert_eq!(rest.text, " bar");
        assert_eq!(rest.anchor, Position::new(3, 7), "moved right by 3 bytes");

        assert_eq!(accept(&mut st, Accept::All).as_deref(), Some(" bar"));
        assert!(st.current().is_none(), "nothing left");
    }

    #[test]
    fn a_multiline_partial_accept_re_anchors_onto_the_next_line() {
        let mut st = InlineSuggest { enabled: true, ..Default::default() };
        st.arm();
        let seq = st.issue();
        assert!(st.fulfill(seq, Position::new(1, 8), "if x {\n    y\n}"));

        assert_eq!(accept(&mut st, Accept::Line).as_deref(), Some("if x {\n"));
        let rest = st.current().expect("the rest is still offered");
        assert_eq!(rest.text, "    y\n}");
        assert_eq!(rest.anchor, Position::new(2, 0));
    }

    #[test]
    fn a_reply_for_a_buffer_that_moved_on_is_dropped() {
        let mut st = InlineSuggest { enabled: true, ..Default::default() };
        st.arm();
        let seq = st.issue();
        // The user kept typing while the model was thinking.
        st.invalidate();
        assert!(!st.fulfill(seq, Position::new(0, 0), "stale"));
        assert!(st.current().is_none());
    }

    #[test]
    fn only_one_request_is_in_flight_at_a_time() {
        let mut st = InlineSuggest { enabled: true, ..Default::default() };
        st.arm();
        assert!(st.wants_request());
        let seq = st.issue();
        assert!(!st.wants_request(), "the first request is still out");
        st.fail(seq);
        assert!(!st.wants_request(), "a failure doesn't re-ask on its own");
        st.invalidate();
        assert!(st.wants_request(), "the next edit does");
    }

    #[test]
    fn a_dismissed_suggestion_is_not_immediately_re_requested() {
        // Rejecting ghost text and having the identical ghost reappear a moment
        // later is the single most irritating failure mode of this feature.
        let mut st = InlineSuggest { enabled: true, ..Default::default() };
        st.arm();
        let seq = st.issue();
        st.fulfill(seq, Position::new(0, 0), "nope");
        st.dismiss();
        assert!(st.current().is_none());
        assert!(!st.wants_request());
    }

    #[test]
    fn nothing_is_requested_while_the_feature_is_off() {
        let mut st = InlineSuggest::default();
        st.invalidate();
        assert!(!st.wants_request());
    }

    #[test]
    fn context_is_sliced_at_the_cursor_and_bounded_by_the_window() {
        let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
        let window = ContextWindow { before: 2, after: 1 };
        let (prefix, suffix) = split_context(&lines, Position::new(5, 4), window);
        assert_eq!(prefix, "line3\nline4\nline");
        assert_eq!(suffix, "5\nline6");
    }

    #[test]
    fn context_survives_a_cursor_past_the_end_of_a_line() {
        // Insert mode rests one column past the last character.
        let lines = vec!["ab".to_string()];
        let (prefix, suffix) = split_context(&lines, Position::new(0, 99), ContextWindow::default());
        assert_eq!(prefix, "ab");
        assert_eq!(suffix, "");
    }

    #[test]
    fn context_never_splits_a_multibyte_character() {
        let lines = vec!["héllo".to_string()];
        // Byte column 2 is inside the two-byte `é`.
        let (prefix, suffix) = split_context(&lines, Position::new(0, 2), ContextWindow::default());
        assert_eq!(prefix, "h");
        assert_eq!(suffix, "éllo");
    }

    #[test]
    fn build_request_carries_the_seq_that_gates_the_reply() {
        let mut st = InlineSuggest { enabled: true, ..Default::default() };
        st.arm();
        let lines = vec!["fn main() {".to_string(), "".to_string()];
        let req = build_request(&mut st, &lines, Position::new(1, 0), Some("m.rs".into()))
            .expect("a request is wanted");
        assert_eq!(req.prefix, "fn main() {\n");
        assert_eq!(req.filename.as_deref(), Some("m.rs"));
        assert!(st.fulfill(req.seq, req.anchor, "    todo!()"));
        assert_eq!(st.current().expect("shown").text, "    todo!()");
    }
}
