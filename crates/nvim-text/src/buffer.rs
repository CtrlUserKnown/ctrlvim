//! Rope-backed buffer text storage — the replacement for Neovim's `memline.c`.
//!
//! Neovim stores buffer text in a swapfile-backed B-tree (`memline_T`). We drop
//! the swapfile/crash-recovery machinery entirely (out of scope per the plan)
//! and back the text with a [`ropey::Rope`], which gives O(log n) line indexing,
//! insertion, and byte↔line conversion — the operations the old B-tree existed
//! to make cheap.
//!
//! Neovim treats a buffer as a sequence of lines with an implicit trailing
//! newline: a file "abc\n" is one line, "abc". We preserve that model — the
//! rope internally always ends with a `\n`, and [`Buffer::line_count`] counts
//! the logical lines (never zero; an empty buffer is a single empty line).

use ropey::Rope;

/// A single buffer's text content.
#[derive(Clone)]
pub struct Buffer {
    rope: Rope,
}

impl Default for Buffer {
    fn default() -> Self {
        Buffer::new()
    }
}

impl Buffer {
    /// An empty buffer: one empty line.
    pub fn new() -> Self {
        Buffer {
            rope: Rope::from_str("\n"),
        }
    }

    /// Build a buffer from raw file contents. A missing trailing newline is
    /// added (Neovim's `'eol'`/`'fixeol'` default behavior); we always keep the
    /// internal invariant that the rope ends in `\n`.
    pub fn from_str(text: &str) -> Self {
        let mut s = text.to_string();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        Buffer {
            rope: Rope::from_str(&s),
        }
    }

    /// Build from a list of lines (no embedded newlines expected).
    pub fn from_lines<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut rope = Rope::new();
        let mut any = false;
        for line in lines {
            any = true;
            rope.insert(rope.len_chars(), line.as_ref());
            rope.insert(rope.len_chars(), "\n");
        }
        if !any {
            rope.insert(0, "\n");
        }
        Buffer { rope }
    }

    /// Number of logical lines. Always >= 1.
    pub fn line_count(&self) -> usize {
        // The rope always ends in `\n`; ropey counts that trailing newline as
        // starting an extra empty line, so subtract 1. "a\n" -> len_lines 2 -> 1.
        self.rope.len_lines().saturating_sub(1).max(1)
    }

    /// Total number of bytes (including the trailing newline).
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    /// Fetch line `idx` (0-based) without its trailing newline. Returns `None`
    /// if out of range. This is the equivalent of `ml_get`.
    pub fn line(&self, idx: usize) -> Option<String> {
        if idx >= self.line_count() {
            return None;
        }
        let slice = self.rope.line(idx);
        let mut s: String = slice.into();
        if s.ends_with('\n') {
            s.pop();
            if s.ends_with('\r') {
                s.pop();
            }
        }
        Some(s)
    }

    /// Byte length of line `idx` excluding the newline (`ml_get_len`).
    pub fn line_len(&self, idx: usize) -> usize {
        self.line(idx).map(|s| s.len()).unwrap_or(0)
    }

    /// All lines as owned strings (used by `nvim_buf_get_lines` and dumps).
    pub fn lines(&self) -> Vec<String> {
        (0..self.line_count())
            .map(|i| self.line(i).unwrap_or_default())
            .collect()
    }

    /// Byte offset of the first character of line `idx` (`line2byte`).
    pub fn line_to_byte(&self, idx: usize) -> usize {
        let idx = idx.min(self.line_count());
        self.rope.line_to_byte(idx)
    }

    /// Line containing byte offset `byte` (`byte2line`).
    pub fn byte_to_line(&self, byte: usize) -> usize {
        let byte = byte.min(self.rope.len_bytes().saturating_sub(1));
        self.rope.byte_to_line(byte)
    }

    /// Replace the whole line range `[start, end)` (0-based, end-exclusive) with
    /// `replacement` lines. This is the core primitive behind
    /// `nvim_buf_set_lines`, `dd`, `o`, paste, etc.
    ///
    /// `start == end` inserts before `start`. Clamps to valid range.
    pub fn set_lines<S: AsRef<str>>(&mut self, start: usize, end: usize, replacement: &[S]) {
        let n = self.line_count();
        let start = start.min(n);
        let end = end.clamp(start, n);

        let start_byte = self.rope.line_to_byte(start);
        let end_byte = self.rope.line_to_byte(end);
        self.rope.remove(
            self.rope.byte_to_char(start_byte)..self.rope.byte_to_char(end_byte),
        );

        let mut insert = String::new();
        for line in replacement {
            insert.push_str(line.as_ref());
            insert.push('\n');
        }
        let char_at = self.rope.byte_to_char(start_byte);
        self.rope.insert(char_at, &insert);

        // Preserve the "always ends in \n, never empty" invariant.
        if self.rope.len_bytes() == 0 {
            self.rope.insert(0, "\n");
        }
    }

    /// Replace a single line's text (`ml_replace`).
    pub fn replace_line(&mut self, idx: usize, text: &str) {
        self.set_lines(idx, idx + 1, &[text]);
    }

    /// Insert `text` at position `(line, col)` (byte column). `text` may contain
    /// newlines. Returns the resulting end position.
    pub fn insert(&mut self, line: usize, col: usize, text: &str) -> (usize, usize) {
        let line = line.min(self.line_count().saturating_sub(1));
        let line_start = self.rope.line_to_byte(line);
        let col = col.min(self.line_len(line));
        let byte = line_start + col;
        let char_idx = self.rope.byte_to_char(byte);
        self.rope.insert(char_idx, text);

        // Compute end position.
        let newlines = text.matches('\n').count();
        if newlines == 0 {
            (line, col + text.len())
        } else {
            let last = text.rsplit('\n').next().unwrap_or("");
            (line + newlines, last.len())
        }
    }

    /// Delete the half-open byte range `[start, end)` expressed as
    /// `(line, col)` pairs. Used by charwise operators.
    pub fn delete_range(&mut self, start: (usize, usize), end: (usize, usize)) {
        let sb = self.rope.line_to_byte(start.0.min(self.line_count() - 1)) + start.1;
        let eb = self.rope.line_to_byte(end.0.min(self.line_count() - 1)) + end.1;
        let (sb, eb) = if sb <= eb { (sb, eb) } else { (eb, sb) };
        let sc = self.rope.byte_to_char(sb.min(self.rope.len_bytes()));
        let ec = self.rope.byte_to_char(eb.min(self.rope.len_bytes()));
        self.rope.remove(sc..ec);
        if self.rope.len_bytes() == 0 || !self.ends_with_newline() {
            self.rope.insert(self.rope.len_chars(), "\n");
        }
    }

    fn ends_with_newline(&self) -> bool {
        let n = self.rope.len_bytes();
        n > 0 && self.rope.byte(n - 1) == b'\n'
    }

    /// Whole buffer as a single string (with trailing newline).
    pub fn to_string(&self) -> String {
        self.rope.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_is_one_empty_line() {
        let b = Buffer::new();
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line(0).as_deref(), Some(""));
    }

    #[test]
    fn from_str_counts_lines() {
        let b = Buffer::from_str("alpha\nbeta\ngamma");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line(0).as_deref(), Some("alpha"));
        assert_eq!(b.line(2).as_deref(), Some("gamma"));
        assert_eq!(b.line(3), None);
    }

    #[test]
    fn set_lines_replace_insert_delete() {
        let mut b = Buffer::from_lines(["a", "b", "c"]);
        b.set_lines(1, 2, &["B1", "B2"]); // replace "b" with two lines
        assert_eq!(b.lines(), vec!["a", "B1", "B2", "c"]);
        b.set_lines(0, 0, &["top"]); // insert at start
        assert_eq!(b.lines(), vec!["top", "a", "B1", "B2", "c"]);
        b.set_lines(1, 4, &[] as &[&str]); // delete range
        assert_eq!(b.lines(), vec!["top", "c"]);
    }

    #[test]
    fn insert_within_line() {
        let mut b = Buffer::from_lines(["hello"]);
        let end = b.insert(0, 5, " world");
        assert_eq!(b.line(0).as_deref(), Some("hello world"));
        assert_eq!(end, (0, 11));
    }

    #[test]
    fn insert_with_newline_splits() {
        let mut b = Buffer::from_lines(["abcdef"]);
        b.insert(0, 3, "\n");
        assert_eq!(b.lines(), vec!["abc", "def"]);
    }

    #[test]
    fn byte_line_roundtrip() {
        let b = Buffer::from_lines(["ab", "cde", "f"]);
        assert_eq!(b.line_to_byte(0), 0);
        assert_eq!(b.line_to_byte(1), 3); // "ab\n"
        assert_eq!(b.line_to_byte(2), 7); // + "cde\n"
        assert_eq!(b.byte_to_line(4), 1);
    }

    #[test]
    fn delete_range_charwise() {
        let mut b = Buffer::from_lines(["hello world"]);
        b.delete_range((0, 5), (0, 11)); // remove " world"
        assert_eq!(b.line(0).as_deref(), Some("hello"));
    }
}
