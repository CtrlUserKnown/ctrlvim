//! Buffer positions and ranges.
//!
//! Neovim's `pos_T` is `{ lnum (1-based), col (0-based byte), coladd }`. The API
//! surface, however, is inconsistent: `ctrlvim_win_get_cursor` returns `(1-based
//! row, 0-based col)` while extmarks are `(0-based row, 0-based col)`. We store
//! everything 0-based internally in [`Position`] and convert at the API edge.

/// A 0-based `(line, col)` position. `col` is a byte offset into the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Position {
    /// 0-based line index.
    pub line: usize,
    /// 0-based byte column within the line.
    pub col: usize,
}

impl Position {
    pub const fn new(line: usize, col: usize) -> Self {
        Position { line, col }
    }

    /// Convert to Neovim's `ctrlvim_win_get_cursor` convention: `(1-based row,
    /// 0-based col)`.
    pub fn to_cursor_api(self) -> (i64, i64) {
        (self.line as i64 + 1, self.col as i64)
    }

    /// Build from Neovim's cursor convention `(1-based row, 0-based col)`.
    pub fn from_cursor_api(row: i64, col: i64) -> Self {
        Position {
            line: (row.max(1) - 1) as usize,
            col: col.max(0) as usize,
        }
    }
}

/// A half-open `[start, end)` range over buffer positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub const fn new(start: Position, end: Position) -> Self {
        Range { start, end }
    }

    /// Normalize so `start <= end`, returning the ordered pair.
    pub fn ordered(self) -> Range {
        if self.start <= self.end {
            self
        } else {
            Range {
                start: self.end,
                end: self.start,
            }
        }
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}
