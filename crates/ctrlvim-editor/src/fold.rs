//! Folds — the model half of `fold.c`.
//!
//! A fold is a range of buffer lines that can be *closed*, collapsing to a
//! single row. That breaks the frontend's otherwise-safe assumption that one
//! buffer line is one screen row, so the mapping between the two lives here and
//! every consumer (rendering, `j`/`k`, scrolling) goes through it rather than
//! doing its own arithmetic.
//!
//! Folds are per **window**, as in Vim: two splits on the same buffer fold
//! independently. `'foldmethod'` decides where they come from — `manual` (`zf`)
//! or `indent` (computed from the text).

use ctrlvim_options::FoldMethod;

/// One fold: an inclusive range of buffer lines plus its nesting level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    pub start: usize,
    /// Inclusive last line of the fold.
    pub end: usize,
    pub closed: bool,
    /// Nesting depth, 1 for a top-level fold (Vim's `'foldlevel'` scale).
    pub level: usize,
}

impl Fold {
    pub fn contains(&self, line: usize) -> bool {
        line >= self.start && line <= self.end
    }

    /// How many lines the fold hides when closed (its first line still shows).
    pub fn hidden_lines(&self) -> usize {
        self.end - self.start
    }
}

/// The folds of one window, kept sorted by start line.
#[derive(Debug, Clone, Default)]
pub struct Folds {
    folds: Vec<Fold>,
    /// `'foldenable'` — `zi`. When off, everything renders unfolded but the
    /// fold definitions survive.
    pub enabled: bool,
}

impl Folds {
    pub fn new() -> Self {
        Folds { folds: Vec::new(), enabled: true }
    }

    pub fn all(&self) -> &[Fold] {
        &self.folds
    }

    pub fn is_empty(&self) -> bool {
        self.folds.is_empty()
    }

    /// Create a fold over `[start, end]` (`zf`, `:fold`). A zero-length or
    /// backwards range is ignored, as Vim ignores `zfj` on the last line.
    ///
    /// Nesting is allowed: a fold created inside an existing one gets the
    /// deeper level.
    pub fn create(&mut self, start: usize, end: usize) {
        if end <= start {
            return;
        }
        let level = self
            .folds
            .iter()
            .filter(|f| f.start <= start && f.end >= end)
            .count()
            + 1;
        self.folds.push(Fold { start, end, closed: true, level });
        self.folds.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
    }

    /// Remove every fold containing `line` (`zd`/`zE` territory).
    pub fn delete_at(&mut self, line: usize) {
        self.folds.retain(|f| !f.contains(line));
    }

    pub fn clear(&mut self) {
        self.folds.clear();
    }

    /// The innermost (deepest, smallest) fold containing `line`.
    pub fn innermost_at(&self, line: usize) -> Option<&Fold> {
        self.folds
            .iter()
            .filter(|f| f.contains(line))
            .min_by_key(|f| f.end - f.start)
    }

    /// The outermost *closed* fold containing `line` — the one that decides
    /// whether the line is visible at all.
    ///
    /// Outermost, because a closed fold hides the ones nested inside it: if an
    /// outer fold is closed, opening the inner one changes nothing on screen.
    pub fn closed_at(&self, line: usize) -> Option<&Fold> {
        if !self.enabled {
            return None;
        }
        self.folds
            .iter()
            .filter(|f| f.closed && f.contains(line))
            .max_by_key(|f| f.end - f.start)
    }

    /// Whether `line` is hidden inside a closed fold (its first line is not —
    /// that is the row the summary is drawn on).
    pub fn is_hidden(&self, line: usize) -> bool {
        self.closed_at(line).is_some_and(|f| f.start != line)
    }

    /// Open, close, or toggle the innermost fold at `line`. Returns whether a
    /// fold was there to act on.
    pub fn set_closed_at(&mut self, line: usize, closed: Option<bool>) -> bool {
        // Act on the innermost fold, but when closing from inside a *hidden*
        // line the outer closed fold is what the user sees.
        let Some(target) = self.innermost_at(line).map(|f| (f.start, f.end)) else {
            return false;
        };
        for fold in &mut self.folds {
            if (fold.start, fold.end) == target {
                fold.closed = match closed {
                    Some(c) => c,
                    None => !fold.closed,
                };
            }
        }
        true
    }

    /// `zR` (open all) / `zM` (close all).
    pub fn set_all_closed(&mut self, closed: bool) {
        for fold in &mut self.folds {
            fold.closed = closed;
        }
    }

    /// Rebuild folds from the buffer for a computed `'foldmethod'`.
    ///
    /// `manual` keeps whatever `zf` created. `indent` derives folds from
    /// indentation, preserving the open/closed state of ranges that survive an
    /// edit so typing inside a fold doesn't make it pop open.
    pub fn recompute(&mut self, method: FoldMethod, lines: &[String], shiftwidth: usize) {
        if method != FoldMethod::Indent {
            return;
        }
        let previous = std::mem::take(&mut self.folds);
        self.folds = indent_folds(lines, shiftwidth.max(1));
        for fold in &mut self.folds {
            if let Some(old) = previous.iter().find(|o| o.start == fold.start && o.end == fold.end) {
                fold.closed = old.closed;
            }
        }
    }

    // --- the screen/buffer line mapping ---

    /// The screen row a buffer line renders on, counting from the top of the
    /// buffer. Lines hidden inside a closed fold map to their fold's row.
    pub fn screen_line_of(&self, line: usize) -> usize {
        let mut screen = 0;
        let mut buffer = 0;
        while buffer < line {
            match self.closed_at(buffer) {
                // Skip the whole fold; it occupies one row.
                Some(fold) if fold.start == buffer => {
                    if fold.end >= line {
                        return screen;
                    }
                    buffer = fold.end + 1;
                }
                _ => buffer += 1,
            }
            screen += 1;
        }
        screen
    }

    /// The buffer line drawn on screen row `screen` (inverse of
    /// [`screen_line_of`](Self::screen_line_of)), clamped to `line_count - 1`.
    pub fn buffer_line_of(&self, screen: usize, line_count: usize) -> usize {
        let mut buffer = 0;
        let mut row = 0;
        while row < screen && buffer < line_count {
            buffer = match self.closed_at(buffer) {
                Some(fold) if fold.start == buffer => fold.end + 1,
                _ => buffer + 1,
            };
            row += 1;
        }
        buffer.min(line_count.saturating_sub(1))
    }

    /// The next visible line in `dir` (+1/-1) from `line` — what `j`/`k` move
    /// to, stepping over a closed fold instead of into it.
    ///
    /// Returns `None` at the buffer's edges.
    pub fn next_visible(&self, line: usize, dir: isize, line_count: usize) -> Option<usize> {
        if line_count == 0 {
            return None;
        }
        let candidate = if dir > 0 {
            // Leaving a closed fold means leaving all of it.
            let from = self.closed_at(line).map(|f| f.end).unwrap_or(line);
            let next = from + 1;
            (next < line_count).then_some(next)
        } else {
            let from = self.closed_at(line).map(|f| f.start).unwrap_or(line);
            from.checked_sub(1)
        }?;
        // Land on the fold's first line, never inside it.
        Some(self.closed_at(candidate).map(|f| f.start).unwrap_or(candidate))
    }

    /// Total screen rows the buffer occupies with folds applied.
    pub fn screen_line_count(&self, line_count: usize) -> usize {
        if line_count == 0 {
            return 0;
        }
        self.screen_line_of(line_count - 1) + 1
    }
}

/// Derive folds from indentation: a run of lines indented deeper than the line
/// above them folds under it, which is `foldmethod=indent` in miniature.
///
/// Blank lines don't break a fold (Vim treats them as belonging to the
/// surrounding level), so a function with blank lines inside stays one fold.
fn indent_folds(lines: &[String], shiftwidth: usize) -> Vec<Fold> {
    let levels: Vec<Option<usize>> = lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                None
            } else {
                Some(indent_width(l) / shiftwidth)
            }
        })
        .collect();

    let mut folds = Vec::new();
    // Open fold starts, as (start line, level).
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut last_level = 0usize;
    let mut last_content = 0usize;

    for (i, level) in levels.iter().enumerate() {
        let Some(level) = *level else { continue };
        if level > last_level {
            // Deeper: the previous content line heads a new fold.
            for depth in last_level..level {
                open.push((last_content, depth + 1));
            }
        } else if level < last_level {
            // Shallower: close every fold deeper than this line.
            while let Some(&(start, depth)) = open.last() {
                if depth <= level {
                    break;
                }
                open.pop();
                if last_content > start {
                    folds.push(Fold { start, end: last_content, closed: false, level: depth });
                }
            }
        }
        last_level = level;
        last_content = i;
    }
    // Close whatever is still open at the end of the buffer.
    while let Some((start, depth)) = open.pop() {
        if last_content > start {
            folds.push(Fold { start, end: last_content, closed: false, level: depth });
        }
    }
    folds.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
    folds
}

/// Indentation of a line in columns (a tab counts as one level's worth, which
/// is what `foldmethod=indent` needs rather than exact tabstop math).
fn indent_width(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// The summary text drawn on a closed fold's row (`'foldtext'`), e.g.
/// `+--  12 lines: fn main() {`.
pub fn fold_text(fold: &Fold, first_line: &str) -> String {
    let dashes = "-".repeat(fold.level.min(4));
    format!(
        "+{}{}  {} lines: {}",
        dashes,
        if fold.level > 4 { "+" } else { "" },
        fold.end - fold.start + 1,
        first_line.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    /// A buffer of 10 lines with one closed fold over lines 2..=5.
    fn folded() -> Folds {
        let mut f = Folds::new();
        f.create(2, 5);
        f
    }

    #[test]
    fn a_closed_fold_collapses_its_lines_to_one_row() {
        let f = folded();
        // Above the fold: rows track lines.
        assert_eq!(f.screen_line_of(0), 0);
        assert_eq!(f.screen_line_of(1), 1);
        // Every line of the fold draws on the fold's own row.
        for line in 2..=5 {
            assert_eq!(f.screen_line_of(line), 2, "line {line} is inside the fold");
        }
        // Below it, rows are shifted up by the hidden lines.
        assert_eq!(f.screen_line_of(6), 3);
        assert_eq!(f.screen_line_of(9), 6);
        assert_eq!(f.screen_line_count(10), 7);
    }

    #[test]
    fn the_mapping_round_trips_for_visible_lines() {
        let f = folded();
        for line in [0, 1, 2, 6, 7, 8, 9] {
            let screen = f.screen_line_of(line);
            assert_eq!(f.buffer_line_of(screen, 10), line, "round trip for line {line}");
        }
        // A hidden line maps onto its fold's first line, not itself.
        assert_eq!(f.buffer_line_of(f.screen_line_of(4), 10), 2);
    }

    #[test]
    fn an_open_fold_changes_nothing() {
        let mut f = folded();
        f.set_closed_at(2, Some(false));
        for line in 0..10 {
            assert_eq!(f.screen_line_of(line), line);
            assert_eq!(f.buffer_line_of(line, 10), line);
        }
        assert!(!f.is_hidden(4));
    }

    #[test]
    fn foldenable_off_shows_everything_without_losing_the_folds() {
        let mut f = folded();
        f.enabled = false;
        assert_eq!(f.screen_line_of(9), 9, "nothing is collapsed");
        assert!(!f.is_hidden(4));
        assert_eq!(f.all().len(), 1, "the fold still exists");
        f.enabled = true;
        assert_eq!(f.screen_line_of(9), 6);
    }

    #[test]
    fn movement_steps_over_a_closed_fold() {
        let f = folded();
        // Down from above the fold lands on its first line…
        assert_eq!(f.next_visible(1, 1, 10), Some(2));
        // …and again jumps clear of the whole thing.
        assert_eq!(f.next_visible(2, 1, 10), Some(6));
        // Upward, symmetrically.
        assert_eq!(f.next_visible(6, -1, 10), Some(2));
        assert_eq!(f.next_visible(2, -1, 10), Some(1));
        // Edges.
        assert_eq!(f.next_visible(0, -1, 10), None);
        assert_eq!(f.next_visible(9, 1, 10), None);
    }

    #[test]
    fn is_hidden_covers_the_fold_body_but_not_its_head() {
        let f = folded();
        assert!(!f.is_hidden(2), "the head draws the summary row");
        assert!(f.is_hidden(3) && f.is_hidden(5));
        assert!(!f.is_hidden(6));
    }

    #[test]
    fn nested_folds_take_the_outer_ones_visibility() {
        let mut f = Folds::new();
        f.create(1, 8); // outer
        f.create(3, 5); // inner
        assert_eq!(f.innermost_at(4).map(|f| (f.start, f.end)), Some((3, 5)));
        // Both closed: the outer decides, so the buffer is head + one row.
        assert_eq!(f.screen_line_of(9), 2);
        // Opening only the inner one changes nothing while the outer is closed.
        f.set_closed_at(4, Some(false));
        assert_eq!(f.screen_line_of(9), 2);
        // Re-close the inner one and open the outer: now the inner fold is the
        // only thing hiding lines (4 and 5), so the buffer is two rows shorter.
        f.set_closed_at(4, Some(true));
        f.set_closed_at(1, Some(false));
        assert_eq!(f.screen_line_of(9), 7);
        // Opening both reveals everything.
        f.set_all_closed(false);
        assert_eq!(f.screen_line_of(9), 9);
    }

    #[test]
    fn toggling_and_bulk_open_close() {
        let mut f = folded();
        assert!(f.set_closed_at(3, None), "toggle from inside the fold");
        assert!(!f.is_hidden(4), "now open");
        f.set_all_closed(true);
        assert!(f.is_hidden(4));
        f.set_all_closed(false);
        assert!(!f.is_hidden(4));
        assert!(!f.set_closed_at(9, None), "no fold there");
    }

    #[test]
    fn creating_a_degenerate_fold_is_ignored() {
        let mut f = Folds::new();
        f.create(3, 3);
        f.create(5, 2);
        assert!(f.is_empty(), "a fold needs at least two lines");
    }

    #[test]
    fn deleting_removes_every_fold_over_a_line() {
        let mut f = Folds::new();
        f.create(1, 8);
        f.create(3, 5);
        f.delete_at(4);
        assert!(f.is_empty());
    }

    #[test]
    fn indent_method_folds_indented_blocks() {
        let src = "fn main() {\n    let x = 1;\n    if x > 0 {\n        println!();\n    }\n}\nfn other() {}\n";
        let mut f = Folds::new();
        f.recompute(FoldMethod::Indent, &lines(src), 4);
        let ranges: Vec<(usize, usize)> = f.all().iter().map(|f| (f.start, f.end)).collect();
        assert!(ranges.contains(&(0, 4)), "the fn body folds under its header: {ranges:?}");
        assert!(ranges.contains(&(2, 3)), "the if body nests inside: {ranges:?}");
        // Computed folds start open, like Vim with foldlevel at its default.
        assert!(f.all().iter().all(|f| !f.closed));
    }

    #[test]
    fn indent_folds_survive_a_recompute_with_their_state() {
        let src = "a\n    b\n    c\nd\n";
        let mut f = Folds::new();
        f.recompute(FoldMethod::Indent, &lines(src), 4);
        f.set_all_closed(true);
        // An edit elsewhere re-derives the same range, which stays closed.
        f.recompute(FoldMethod::Indent, &lines("a\n    b\n    c\nd!\n"), 4);
        assert!(f.all()[0].closed, "closed state survives the recompute");
    }

    #[test]
    fn manual_folds_are_never_recomputed_away() {
        let mut f = Folds::new();
        f.create(1, 4);
        f.recompute(FoldMethod::Manual, &lines("a\nb\nc\nd\ne\n"), 4);
        assert_eq!(f.all().len(), 1, "manual folds are the user's, not derived");
    }

    #[test]
    fn fold_text_summarizes_the_range() {
        let fold = Fold { start: 2, end: 13, closed: true, level: 1 };
        assert_eq!(fold_text(&fold, "fn main() {"), "+-  12 lines: fn main() {");
    }

    #[test]
    fn an_empty_buffer_maps_without_panicking() {
        let f = Folds::new();
        assert_eq!(f.screen_line_count(0), 0);
        assert_eq!(f.buffer_line_of(5, 0), 0);
        assert_eq!(f.next_visible(0, 1, 0), None);
    }
}
