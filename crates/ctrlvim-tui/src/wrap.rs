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

use ctrlvim_core::{char_index_at, char_width, width_upto, Folds};

/// One screen row's content: a buffer line, and the char index its slice
/// starts at (0 unless this is a wrapped continuation row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisRow {
    pub line: usize,
    pub seg_start: usize,
    /// True when this row is a closed fold's one-line summary.
    pub fold_head: bool,
}

/// Char indices where `line` breaks across rows `content_w` cells wide.
/// Always starts with `0`. Breaks are purely width-based (Vim's `'wrap'`
/// without `'linebreak'`), so a break can land mid-word.
pub fn wrap_starts(line: &str, content_w: usize) -> Vec<usize> {
    if content_w == 0 {
        return vec![0];
    }
    let mut starts = vec![0usize];
    let mut width = 0usize;
    for (i, c) in line.chars().enumerate() {
        let w = char_width(c);
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
pub fn visual_rows(lines: &[String], folds: &Folds, content_w: usize, wrap: bool, line_count: usize) -> Vec<VisRow> {
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
                    for s in wrap_starts(text, content_w) {
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
) -> Viewport {
    let rows = visual_rows(lines, folds, content_w, wrap, lines.len());
    let height = height.max(1);
    let cur_row = row_of(&rows, cursor_line, cursor_col);
    let top_row = view_top.clamp(cur_row.saturating_sub(height - 1), cur_row);

    let left_cells = if wrap {
        0
    } else {
        let raw = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let cur_cells = width_upto(raw, cursor_col);
        let content_w = content_w.max(1);
        view_left.clamp(cur_cells.saturating_sub(content_w - 1), cur_cells)
    };

    Viewport { rows, top_row, left_cells }
}

/// The char index the row starting at screen cell `left_cells` begins its
/// slice at, for `'nowrap'` horizontal scrolling.
pub fn left_char(line: &str, left_cells: usize) -> usize {
    char_index_at(line, left_cells)
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
        assert_eq!(wrap_starts("abcdefgh", 3), vec![0, 3, 6]);
        assert_eq!(wrap_starts("abc", 3), vec![0]);
        assert_eq!(wrap_starts("", 3), vec![0]);
    }

    #[test]
    fn visual_rows_expands_wrapped_lines() {
        let lines = vec!["abcdefgh".to_string(), "hi".to_string()];
        let rows = visual_rows(&lines, &folds(), 3, true, 2);
        assert_eq!(rows.len(), 4); // 3 wrap segments + 1 short line
        assert_eq!(rows[0], VisRow { line: 0, seg_start: 0, fold_head: false });
        assert_eq!(rows[1], VisRow { line: 0, seg_start: 3, fold_head: false });
        assert_eq!(rows[2], VisRow { line: 0, seg_start: 6, fold_head: false });
        assert_eq!(rows[3], VisRow { line: 1, seg_start: 0, fold_head: false });
    }

    #[test]
    fn visual_rows_one_row_per_line_when_nowrap() {
        let lines = vec!["abcdefgh".to_string(), "short".to_string()];
        let rows = visual_rows(&lines, &folds(), 3, false, 2);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn row_of_finds_wrapped_segment() {
        let lines = vec!["abcdefgh".to_string()];
        let rows = visual_rows(&lines, &folds(), 3, true, 1);
        assert_eq!(row_of(&rows, 0, 0), 0);
        assert_eq!(row_of(&rows, 0, 3), 1);
        assert_eq!(row_of(&rows, 0, 7), 2);
    }

    #[test]
    fn compute_scrolls_left_to_keep_cursor_visible() {
        let lines = vec!["0123456789".to_string()];
        let vp = compute(&lines, &folds(), false, 4, 10, 0, 9, 0, 0);
        assert_eq!(vp.left_cells, 6); // cursor at cell 9, width 4 -> left = 9-3
    }

    #[test]
    fn compute_no_horizontal_scroll_when_wrapped() {
        let lines = vec!["0123456789".to_string()];
        let vp = compute(&lines, &folds(), true, 4, 10, 0, 9, 0, 0);
        assert_eq!(vp.left_cells, 0);
    }
}
