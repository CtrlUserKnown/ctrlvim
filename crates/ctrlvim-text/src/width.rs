//! Display width — the replacement for the width half of `mbyte.c`.
//!
//! Buffer columns are *character* indices; screen columns are *cells*. For
//! ASCII those coincide, which is why a renderer can get away with treating
//! them as the same number until the first CJK ideograph or emoji shows up —
//! at which point the cursor, the selection band, and the search highlight all
//! drift right by one cell per wide glyph already passed.
//!
//! Everything here converts between the two. Tabs are not handled at this
//! level: their width depends on `'tabstop'` and the column they start at, so
//! callers that care pass expanded text.

use unicode_width::UnicodeWidthChar;

/// Screen cells occupied by one character.
///
/// Control characters report 0 from `unicode-width`; we report 1 so a stray
/// control byte still occupies the cell a terminal will actually paint.
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(1)
}

/// Total screen width of a string.
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Screen column at which the character with index `char_idx` starts.
///
/// This is the conversion a renderer needs to place a cursor: given "日本語x"
/// and char index 3, the answer is 6, not 3.
pub fn width_upto(s: &str, char_idx: usize) -> usize {
    s.chars().take(char_idx).map(char_width).sum()
}

/// Inverse of [`width_upto`]: the character index whose cell span contains
/// screen column `col`. Used when translating a mouse click back to a buffer
/// position. A column past the end of the line yields the line's char count.
pub fn char_index_at(s: &str, col: usize) -> usize {
    let mut cells = 0;
    for (i, c) in s.chars().enumerate() {
        let w = char_width(c);
        if cells + w > col {
            return i;
        }
        cells += w;
    }
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_columns_and_cells_coincide() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(width_upto("hello", 3), 3);
        assert_eq!(char_index_at("hello", 3), 3);
    }

    #[test]
    fn cjk_takes_two_cells_each() {
        // Three ideographs then an ASCII char.
        let s = "日本語x";
        assert_eq!(display_width(s), 7);
        assert_eq!(width_upto(s, 0), 0);
        assert_eq!(width_upto(s, 1), 2);
        assert_eq!(width_upto(s, 3), 6, "the ASCII char starts at cell 6");
    }

    #[test]
    fn a_click_inside_a_wide_glyph_selects_that_glyph() {
        let s = "日本語";
        // Both cells of the first ideograph map back to char 0.
        assert_eq!(char_index_at(s, 0), 0);
        assert_eq!(char_index_at(s, 1), 0);
        assert_eq!(char_index_at(s, 2), 1);
        // Past the end clamps to the char count.
        assert_eq!(char_index_at(s, 99), 3);
    }

    #[test]
    fn combining_marks_take_no_cells() {
        // "e" + combining acute renders in one cell.
        let s = "e\u{0301}";
        assert_eq!(display_width(s), 1);
    }

    #[test]
    fn emoji_are_wide() {
        assert_eq!(char_width('🦀'), 2);
        assert_eq!(width_upto("🦀x", 1), 2);
    }
}
