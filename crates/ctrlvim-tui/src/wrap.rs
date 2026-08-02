//! Screen-row math for the file-buffer view, layered on top of [`Folds`]'s
//! buffer-line/screen-row mapping.
//!
//! `Folds` already collapses a closed fold's hidden lines to one row; this
//! module adds the other half of Vim's row model — `'wrap'` splitting a long
//! line across several rows, and `'nowrap'` scrolling it sideways instead —
//! without touching the model crate, since content width is a UI concern.
//!
//! Everything here is a pure function of the buffer text plus a `content_w`
//! the renderer supplies, mirroring the rest of this frame's per-draw work
//! (the buffer is already copied whole every frame; see [`crate::app::App::editor_lines`]).

use ctrlvim_core::{char_width, Folds};

/// One screen row's content: a buffer line, and the char index its slice
/// starts at (0 unless this is a wrapped continuation row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisRow {
    pub line: usize,
    pub seg_start: usize,
    /// True when this row is a closed fold's one-line summary.
    pub fold_head: bool,
}

/// Cells one character occupies, starting at screen column `col`.
///
/// [`ctrlvim_core::char_width`] deliberately leaves tabs alone (their width
/// depends on `'tabstop'` and the column they start at, which it has no way
/// to know) — this is the "caller that cares" its docs point to. Every width
/// calculation in this module and in the file-buffer renderer that touches a
/// *raw* buffer line (as opposed to already-rendered, tab-free text) must go
/// through this rather than `char_width` directly, or a `\t` silently reverts
/// to costing one cell, which is what desyncs the cursor/selection painting
/// from where the terminal actually puts the text.
pub fn cell_width(c: char, col: usize, tabstop: usize) -> usize {
    if c == '\t' {
        let tabstop = tabstop.max(1);
        tabstop - col % tabstop
    } else {
        char_width(c)
    }
}

/// Tab-aware [`ctrlvim_core::width_upto`]: screen cells `line`'s first
/// `char_idx` characters occupy.
pub fn width_upto_tabs(line: &str, char_idx: usize, tabstop: usize) -> usize {
    let mut col = 0usize;
    for c in line.chars().take(char_idx) {
        col += cell_width(c, col, tabstop);
    }
    col
}

/// Tab-aware [`ctrlvim_core::char_index_at`]: the char index whose cell span
/// contains screen column `col`.
pub fn char_index_at_tabs(line: &str, col: usize, tabstop: usize) -> usize {
    let mut cells = 0usize;
    for (i, c) in line.chars().enumerate() {
        let w = cell_width(c, cells, tabstop);
        if cells + w > col {
            return i;
        }
        cells += w;
    }
    line.chars().count()
}

/// Char indices where `line` breaks across rows `content_w` cells wide.
/// Always starts with `0`. Breaks are purely width-based (Vim's `'wrap'`
/// without `'linebreak'`), so a break can land mid-word.
pub fn wrap_starts(line: &str, content_w: usize, tabstop: usize) -> Vec<usize> {
    if content_w == 0 {
        return vec![0];
    }
    let mut starts = vec![0usize];
    let mut width = 0usize;
    for (i, c) in line.chars().enumerate() {
        let w = cell_width(c, width, tabstop);
        if width + w > content_w {
            starts.push(i);
            width = 0;
        }
        width += w;
    }
    starts
}

/// Every screen row of the buffer, in order, given the window's fold state
/// and `'wrap'` setting. With `wrap` off (or `content_w == 0`) every visible
/// buffer line is exactly one row, same as before this module existed —
/// horizontal movement is handled by scrolling `left_cells` instead.
pub fn visual_rows(
    lines: &[String],
    folds: &Folds,
    content_w: usize,
    wrap: bool,
    line_count: usize,
    tabstop: usize,
) -> Vec<VisRow> {
    let mut out = Vec::with_capacity(line_count);
    let mut line = 0usize;
    while line < line_count {
        match folds.closed_at(line) {
            Some(fold) if fold.start == line => {
                out.push(VisRow { line, seg_start: 0, fold_head: true });
                line = fold.end + 1;
            }
            _ => {
                let text = lines.get(line).map(String::as_str).unwrap_or("");
                if wrap {
                    for s in wrap_starts(text, content_w, tabstop) {
                        out.push(VisRow { line, seg_start: s, fold_head: false });
                    }
                } else {
                    out.push(VisRow { line, seg_start: 0, fold_head: false });
                }
                line += 1;
            }
        }
    }
    out
}

/// The row in `rows` that shows buffer position `(line, col)` — the last
/// segment of `line` whose `seg_start` is at or before `col`. Falls back to
/// the first row for a line that isn't found (shouldn't happen: the cursor is
/// never left on a hidden line).
pub fn row_of(rows: &[VisRow], line: usize, col: usize) -> usize {
    let mut best = None;
    for (i, r) in rows.iter().enumerate() {
        if r.line != line {
            if best.is_some() {
                break;
            }
            continue;
        }
        if r.fold_head || r.seg_start <= col {
            best = Some(i);
        } else {
            break;
        }
    }
    best.unwrap_or(0)
}

/// The resolved viewport for one frame: which rows are on screen and, for
/// `'nowrap'`, how far the view is scrolled sideways.
pub struct Viewport {
    pub rows: Vec<VisRow>,
    /// Index into `rows` of the first visible row.
    pub top_row: usize,
    /// Leftmost visible screen cell, in `'nowrap'` mode (always 0 with wrap).
    pub left_cells: usize,
}

impl Viewport {
    /// The char index one past the end of `rows[row]`'s slice: the next
    /// segment's start if the following row continues the same line,
    /// otherwise the line's full length.
    pub fn seg_end(&self, row: usize, lines: &[String]) -> usize {
        let vr = self.rows[row];
        match self.rows.get(row + 1) {
            Some(next) if next.line == vr.line && !next.fold_head => next.seg_start,
            _ => lines.get(vr.line).map(|l| l.chars().count()).unwrap_or(0),
        }
    }
}

/// Build this frame's viewport. `view_top`/`view_left` are the sticky
/// scroll-offsets the mouse wheel moves (in rows / cells); both get clamped
/// here against the cursor so keyboard movement scrolls the view without the
/// app having to track it — the same trick the old vertical-only code used.
///
/// `scrolloff` is Vim's `'scrolloff'`: how many rows to keep between the
/// cursor and the top/bottom edge. It is measured in *screen* rows, not buffer
/// lines, so a wrapped line's segments each count — which is what Vim does
/// too.
#[allow(clippy::too_many_arguments)]
pub fn compute(
    lines: &[String],
    folds: &Folds,
    wrap: bool,
    content_w: usize,
    height: usize,
    cursor_line: usize,
    cursor_col: usize,
    view_top: usize,
    view_left: usize,
    scrolloff: usize,
    tabstop: usize,
) -> Viewport {
    let rows = visual_rows(lines, folds, content_w, wrap, lines.len(), tabstop);
    let height = height.max(1);
    let cur_row = row_of(&rows, cursor_line, cursor_col);
    // A margin wider than half the window can't be honored on both sides at
    // once, so Vim shrinks it to fit — the same clamp the engine's
    // `Window::scroll_to_cursor` applies.
    let so = scrolloff.min(height.saturating_sub(1) / 2);
    // The window of legal `top_row` values. `hi` keeps `so` rows above the
    // cursor, `lo` keeps `so` below it. `lo` additionally refuses to scroll
    // past the last row: near the end of the buffer Vim lets the margin
    // shrink rather than reveal blank space below the text.
    let hi = cur_row.saturating_sub(so);
    let max_top = rows.len().saturating_sub(height);
    // `.min(hi)` keeps the range non-empty for `clamp`, which panics if its
    // bounds cross — they can't here (`so <= (height-1)/2` guarantees it), but
    // the renderer is the wrong place to find out we were wrong about that.
    let lo = (cur_row + so).saturating_sub(height - 1).min(max_top).min(hi);
    let top_row = view_top.clamp(lo, hi);

    let left_cells = if wrap {
        0
    } else {
        let raw = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let cur_cells = width_upto_tabs(raw, cursor_col, tabstop);
        let content_w = content_w.max(1);
        view_left.clamp(cur_cells.saturating_sub(content_w - 1), cur_cells)
    };

    Viewport { rows, top_row, left_cells }
}

/// The char index the row starting at screen cell `left_cells` begins its
/// slice at, for `'nowrap'` horizontal scrolling.
pub fn left_char(line: &str, left_cells: usize, tabstop: usize) -> usize {
    char_index_at_tabs(line, left_cells, tabstop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctrlvim_core::Folds;

    fn folds() -> Folds {
        Folds::new()
    }

    #[test]
    fn wrap_starts_breaks_on_width() {
        assert_eq!(wrap_starts("abcdefgh", 3, 8), vec![0, 3, 6]);
        assert_eq!(wrap_starts("abc", 3, 8), vec![0]);
        assert_eq!(wrap_starts("", 3, 8), vec![0]);
    }

    #[test]
    fn a_tab_costs_the_cells_to_the_next_stop_not_one() {
        // "\tx" at tabstop 4: the tab fills columns 0-3, "x" lands at 4 — five
        // cells total, not the two you'd get by treating '\t' as one cell.
        assert_eq!(width_upto_tabs("\tx", 2, 4), 5);
        // A tab that starts mid-stop only pays for what's left of it.
        assert_eq!(width_upto_tabs("ab\t", 3, 4), 4);
        assert_eq!(width_upto_tabs("abcd\t", 5, 4), 8);
    }

    #[test]
    fn char_index_at_tabs_is_the_inverse() {
        // "\tx" at tabstop 4: the tab spans cells 0-3, "x" sits at cell 4.
        for col in 0..4 {
            assert_eq!(char_index_at_tabs("\tx", col, 4), 0, "column {col} is inside the tab");
        }
        assert_eq!(char_index_at_tabs("\tx", 4, 4), 1, "column 4 is the x");
        // Past the end clamps to the char count, same as the plain version.
        assert_eq!(char_index_at_tabs("\tx", 5, 4), 2);
    }

    #[test]
    fn wrap_starts_accounts_for_tab_width() {
        // At tabstop 4, "\t" occupies cells 0-3, so a 4-wide row holds only
        // the tab — "ab" has nowhere left and starts the next row.
        assert_eq!(wrap_starts("\tab", 4, 4), vec![0, 1]);
    }

    #[test]
    fn visual_rows_expands_wrapped_lines() {
        let lines = vec!["abcdefgh".to_string(), "hi".to_string()];
        let rows = visual_rows(&lines, &folds(), 3, true, 2, 8);
        assert_eq!(rows.len(), 4); // 3 wrap segments + 1 short line
        assert_eq!(rows[0], VisRow { line: 0, seg_start: 0, fold_head: false });
        assert_eq!(rows[1], VisRow { line: 0, seg_start: 3, fold_head: false });
        assert_eq!(rows[2], VisRow { line: 0, seg_start: 6, fold_head: false });
        assert_eq!(rows[3], VisRow { line: 1, seg_start: 0, fold_head: false });
    }

    #[test]
    fn visual_rows_one_row_per_line_when_nowrap() {
        let lines = vec!["abcdefgh".to_string(), "short".to_string()];
        let rows = visual_rows(&lines, &folds(), 3, false, 2, 8);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn row_of_finds_wrapped_segment() {
        let lines = vec!["abcdefgh".to_string()];
        let rows = visual_rows(&lines, &folds(), 3, true, 1, 8);
        assert_eq!(row_of(&rows, 0, 0), 0);
        assert_eq!(row_of(&rows, 0, 3), 1);
        assert_eq!(row_of(&rows, 0, 7), 2);
    }

    #[test]
    fn compute_scrolls_left_to_keep_cursor_visible() {
        let lines = vec!["0123456789".to_string()];
        let vp = compute(&lines, &folds(), false, 4, 10, 0, 9, 0, 0, 0, 8);
        assert_eq!(vp.left_cells, 6); // cursor at cell 9, width 4 -> left = 9-3
    }

    #[test]
    fn compute_no_horizontal_scroll_when_wrapped() {
        let lines = vec!["0123456789".to_string()];
        let vp = compute(&lines, &folds(), true, 4, 10, 0, 9, 0, 0, 0, 8);
        assert_eq!(vp.left_cells, 0);
    }

    /// 40 single-char lines, so screen rows and buffer lines are 1:1.
    fn numbered(n: usize) -> Vec<String> {
        (0..n).map(|i| i.to_string()).collect()
    }

    #[test]
    fn scrolloff_keeps_a_margin_below_the_cursor() {
        let lines = numbered(40);
        // Cursor on line 12, 10-row window, margin 3: the cursor may sit no
        // lower than row 6 of the window, so the view starts at 12+3-9 = 6.
        let vp = compute(&lines, &folds(), false, 20, 10, 12, 0, 0, 0, 3, 8);
        assert_eq!(vp.top_row, 6);
        // Without the margin the cursor is allowed onto the last row.
        let vp = compute(&lines, &folds(), false, 20, 10, 12, 0, 0, 0, 0, 8);
        assert_eq!(vp.top_row, 3);
    }

    #[test]
    fn scrolloff_keeps_a_margin_above_the_cursor() {
        let lines = numbered(40);
        // Scrolled to row 20 by the wheel, cursor at line 22, margin 3: the
        // view has to back off to 19 so three rows stay above the cursor.
        let vp = compute(&lines, &folds(), false, 20, 10, 22, 0, 20, 0, 3, 8);
        assert_eq!(vp.top_row, 19);
    }

    #[test]
    fn scrolloff_is_not_honored_at_the_top_of_the_buffer() {
        let lines = numbered(40);
        // There is nothing above line 1 to show, so the margin just shrinks
        // rather than scrolling to a negative row.
        let vp = compute(&lines, &folds(), false, 20, 10, 1, 0, 0, 0, 8, 8);
        assert_eq!(vp.top_row, 0);
    }

    #[test]
    fn scrolloff_never_scrolls_past_the_last_row() {
        let lines = numbered(20);
        // Cursor on the final line with a big margin: Vim shows the last row
        // at the bottom of the window instead of scrolling into blank space.
        let vp = compute(&lines, &folds(), false, 20, 10, 19, 0, 0, 0, 5, 8);
        assert_eq!(vp.top_row, 10); // 20 rows - 10 high
    }

    #[test]
    fn scrolloff_larger_than_half_the_window_is_clamped_to_fit() {
        let lines = numbered(40);
        // Margin 99 in a 9-row window can't hold on both sides; it becomes 4,
        // which centers the cursor. Crucially this must not panic.
        let vp = compute(&lines, &folds(), false, 20, 9, 20, 0, 0, 0, 99, 8);
        assert_eq!(vp.top_row, 16);
    }
}
