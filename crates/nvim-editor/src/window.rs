//! Windows and the split/frame tree — the *model* half of `window.c`.
//!
//! Per the plan, we keep only the renderer-agnostic model state a UI needs and
//! drop the ~290 display-cache fields of `win_T` (`w_grid`, `w_lines[]`,
//! `w_redr_*`, screen-row caches). The recursive split geometry is preserved as
//! a [`Frame`] tree, which is the one genuinely reusable algorithm in the file.

use nvim_options::WindowOptions;
use nvim_types::{BufferId, Position, WindowId};

/// A window: a viewport onto a buffer. This is pure model state.
pub struct Window {
    pub buffer: BufferId,
    /// Cursor position within the buffer (the C `w_cursor`).
    pub cursor: Position,
    /// Preferred column for vertical motions (`w_curswant`).
    pub curswant: usize,
    /// First visible buffer line (`w_topline`).
    pub topline: usize,
    /// Leftmost visible byte column when `nowrap` (`w_leftcol`).
    pub leftcol: usize,
    /// Viewport height in text rows — needed for `<C-f>`/`<C-d>`/`zz` and
    /// relative-number math even though we do not render.
    pub height: usize,
    /// Viewport width in columns.
    pub width: usize,
    pub options: WindowOptions,
}

impl Window {
    pub fn new(buffer: BufferId) -> Self {
        Window {
            buffer,
            cursor: Position::default(),
            curswant: 0,
            topline: 0,
            leftcol: 0,
            height: 24,
            width: 80,
            options: WindowOptions::default(),
        }
    }

    /// Scroll so the cursor stays within `scrolloff` of the viewport edges,
    /// adjusting `topline`. Returns the (possibly unchanged) topline. This is
    /// the model-side of `update_topline`, with no drawing.
    pub fn scroll_to_cursor(&mut self, scrolloff: usize, line_count: usize) -> usize {
        let so = scrolloff.min(self.height.saturating_sub(1) / 2);
        let cursor_line = self.cursor.line;
        // Cursor above viewport (+ scrolloff).
        if cursor_line < self.topline + so {
            self.topline = cursor_line.saturating_sub(so);
        }
        // Cursor below viewport (- scrolloff).
        let bottom = self.topline + self.height.saturating_sub(1);
        if cursor_line + so > bottom {
            let new_top = cursor_line + so + 1 - self.height;
            self.topline = new_top.min(line_count.saturating_sub(1));
        }
        self.topline
    }
}

/// A node in the split layout tree (`frame_T`). Leaves hold a window; `Row`
/// splits stack children left-to-right (vertical splits), `Col` splits stack
/// them top-to-bottom (horizontal splits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Leaf(WindowId),
    Row(Vec<Frame>),
    Col(Vec<Frame>),
}

impl Frame {
    /// Distribute a total size among `n` children as evenly as Neovim does,
    /// giving the remainder to the earlier children. This is the core of
    /// `frame_new_height`/`frame_new_width`.
    pub fn distribute(total: usize, n: usize) -> Vec<usize> {
        if n == 0 {
            return Vec::new();
        }
        let base = total / n;
        let extra = total % n;
        (0..n)
            .map(|i| base + if i < extra { 1 } else { 0 })
            .collect()
    }

    /// Compute the screen rectangle for every leaf window given the outer
    /// rectangle `(x, y, w, h)`. Splits share space; a Row divides width, a Col
    /// divides height. Returns `(WindowId, x, y, w, h)` per leaf.
    pub fn layout(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<(WindowId, usize, usize, usize, usize)> {
        match self {
            Frame::Leaf(id) => vec![(*id, x, y, w, h)],
            Frame::Row(children) => {
                let widths = Self::distribute(w, children.len());
                let mut out = Vec::new();
                let mut cx = x;
                for (child, cw) in children.iter().zip(widths) {
                    out.extend(child.layout(cx, y, cw, h));
                    cx += cw;
                }
                out
            }
            Frame::Col(children) => {
                let heights = Self::distribute(h, children.len());
                let mut out = Vec::new();
                let mut cy = y;
                for (child, ch) in children.iter().zip(heights) {
                    out.extend(child.layout(x, cy, w, ch));
                    cy += ch;
                }
                out
            }
        }
    }

    /// Collect all window ids in left-to-right, top-to-bottom order.
    pub fn windows(&self) -> Vec<WindowId> {
        match self {
            Frame::Leaf(id) => vec![*id],
            Frame::Row(children) | Frame::Col(children) => {
                children.iter().flat_map(|c| c.windows()).collect()
            }
        }
    }

    /// Remove `target` from the tree, collapsing any split that is left with a
    /// single child. Returns `None` if the whole tree becomes empty (the last
    /// window was removed) — the caller must prevent that.
    pub fn without(&self, target: WindowId) -> Option<Frame> {
        match self {
            Frame::Leaf(id) => {
                if *id == target {
                    None
                } else {
                    Some(Frame::Leaf(*id))
                }
            }
            Frame::Row(children) => Self::collapse(
                children.iter().filter_map(|c| c.without(target)).collect(),
                true,
            ),
            Frame::Col(children) => Self::collapse(
                children.iter().filter_map(|c| c.without(target)).collect(),
                false,
            ),
        }
    }

    fn collapse(kept: Vec<Frame>, row: bool) -> Option<Frame> {
        match kept.len() {
            0 => None,
            1 => Some(kept.into_iter().next().unwrap()),
            _ => Some(if row { Frame::Row(kept) } else { Frame::Col(kept) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribute_gives_remainder_to_earlier() {
        assert_eq!(Frame::distribute(80, 3), vec![27, 27, 26]);
        assert_eq!(Frame::distribute(10, 2), vec![5, 5]);
        assert_eq!(Frame::distribute(5, 0), Vec::<usize>::new());
    }

    #[test]
    fn layout_three_way_vertical_split() {
        let f = Frame::Row(vec![
            Frame::Leaf(WindowId(1)),
            Frame::Leaf(WindowId(2)),
            Frame::Leaf(WindowId(3)),
        ]);
        let rects = f.layout(0, 0, 90, 24);
        assert_eq!(rects.len(), 3);
        assert_eq!(rects[0], (WindowId(1), 0, 0, 30, 24));
        assert_eq!(rects[1], (WindowId(2), 30, 0, 30, 24));
        assert_eq!(rects[2], (WindowId(3), 60, 0, 30, 24));
    }

    #[test]
    fn nested_layout() {
        // Left window, right column split into two.
        let f = Frame::Row(vec![
            Frame::Leaf(WindowId(1)),
            Frame::Col(vec![Frame::Leaf(WindowId(2)), Frame::Leaf(WindowId(3))]),
        ]);
        let rects = f.layout(0, 0, 80, 24);
        assert_eq!(rects[0], (WindowId(1), 0, 0, 40, 24));
        assert_eq!(rects[1], (WindowId(2), 40, 0, 40, 12));
        assert_eq!(rects[2], (WindowId(3), 40, 12, 40, 12));
    }

    #[test]
    fn scroll_keeps_cursor_visible() {
        let mut w = Window::new(BufferId(0));
        w.height = 10;
        w.cursor = Position::new(50, 0);
        w.scroll_to_cursor(0, 100);
        assert!(w.topline <= 50 && 50 <= w.topline + 9);
    }
}
