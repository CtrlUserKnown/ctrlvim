//! Undo tree — the replacement for `undo.c`.
//!
//! Neovim's undo is a genuine tree (not a stack): after you undo and then make
//! a new edit, the old "future" survives as an alternate branch reachable via
//! `g-`/`g+` (time-ordered) and `:undolist`. We model it as an arena
//! (`Vec<UndoNode>`) with index links instead of the C code's raw-pointer /
//! seq-number union, so serialization (deferred) can use indices uniformly.
//!
//! Each node snapshots the full line contents plus the cursor position, matching
//! `u_header_T`'s per-header cursor/mark snapshot. This is a simple, correct
//! whole-buffer snapshot model; a future optimization can store line-range
//! diffs like the C `u_entry_T` does.

use crate::buffer::Buffer;
use ctrlvim_types::Position;

/// Index into the undo arena.
pub type NodeId = usize;

#[derive(Clone)]
struct UndoNode {
    /// Buffer line contents at this point in history.
    lines: Vec<String>,
    cursor: Position,
    /// Parent in the tree (the state this one was derived from). `None` for root.
    parent: Option<NodeId>,
    /// Children, in creation order. The last child is the most recent branch.
    children: Vec<NodeId>,
    /// Monotonic sequence number = creation order, used for `g-`/`g+`.
    seq: u64,
}

/// The undo tree for one buffer.
pub struct UndoTree {
    nodes: Vec<UndoNode>,
    /// The node representing the current buffer state.
    current: NodeId,
    next_seq: u64,
}

impl UndoTree {
    /// Create a tree rooted at the buffer's current contents.
    pub fn new(buf: &Buffer, cursor: Position) -> Self {
        let root = UndoNode {
            lines: buf.lines(),
            cursor,
            parent: None,
            children: Vec::new(),
            seq: 0,
        };
        UndoTree {
            nodes: vec![root],
            current: 0,
            next_seq: 1,
        }
    }

    /// Record a new state as a child of the current node and move to it. Call
    /// this *after* applying an edit (the buffer already holds the new text).
    pub fn commit(&mut self, buf: &Buffer, cursor: Position) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let id = self.nodes.len();
        self.nodes.push(UndoNode {
            lines: buf.lines(),
            cursor,
            parent: Some(self.current),
            children: Vec::new(),
            seq,
        });
        self.nodes[self.current].children.push(id);
        self.current = id;
    }

    /// `u`: move to the parent state. Returns the lines+cursor to restore, or
    /// `None` if already at the root.
    pub fn undo(&mut self) -> Option<(Vec<String>, Position)> {
        let parent = self.nodes[self.current].parent?;
        self.current = parent;
        let n = &self.nodes[parent];
        Some((n.lines.clone(), n.cursor))
    }

    /// `<C-r>`: redo along the most recently created branch (the last child).
    pub fn redo(&mut self) -> Option<(Vec<String>, Position)> {
        let child = *self.nodes[self.current].children.last()?;
        self.current = child;
        let n = &self.nodes[child];
        Some((n.lines.clone(), n.cursor))
    }

    /// `g-`: move to the state with the next-lower sequence number (older in
    /// absolute time), regardless of branch. This is what makes undo a tree
    /// rather than a stack.
    pub fn undo_time(&mut self) -> Option<(Vec<String>, Position)> {
        let cur_seq = self.nodes[self.current].seq;
        let target = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.seq < cur_seq)
            .max_by_key(|(_, n)| n.seq)
            .map(|(i, _)| i)?;
        self.current = target;
        let n = &self.nodes[target];
        Some((n.lines.clone(), n.cursor))
    }

    /// `g+`: move to the state with the next-higher sequence number (newer in
    /// absolute time), regardless of branch.
    pub fn redo_time(&mut self) -> Option<(Vec<String>, Position)> {
        let cur_seq = self.nodes[self.current].seq;
        let target = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.seq > cur_seq)
            .min_by_key(|(_, n)| n.seq)
            .map(|(i, _)| i)?;
        self.current = target;
        let n = &self.nodes[target];
        Some((n.lines.clone(), n.cursor))
    }

    /// Current sequence number (`:undolist`/`b_u_seq_cur`).
    pub fn current_seq(&self) -> u64 {
        self.nodes[self.current].seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[&str]) -> Buffer {
        Buffer::from_lines(lines)
    }

    #[test]
    fn linear_undo_redo() {
        let b0 = buf(&["a"]);
        let mut t = UndoTree::new(&b0, Position::new(0, 0));
        let b1 = buf(&["a", "b"]);
        t.commit(&b1, Position::new(1, 0));
        let (lines, _) = t.undo().unwrap();
        assert_eq!(lines, vec!["a"]);
        let (lines, _) = t.redo().unwrap();
        assert_eq!(lines, vec!["a", "b"]);
        assert!(t.redo().is_none());
    }

    #[test]
    fn branching_preserves_old_future_via_time_travel() {
        // root "a" -> edit1 "ab"; undo; edit2 "ac" (new branch).
        let mut t = UndoTree::new(&buf(&["a"]), Position::default());
        t.commit(&buf(&["ab"]), Position::default()); // seq 1
        t.undo(); // back to "a"
        t.commit(&buf(&["ac"]), Position::default()); // seq 2, new branch

        // redo along newest branch gives nothing further
        assert!(t.redo().is_none());
        // g- steps back in absolute time: seq2 -> seq1 ("ab"), the old future
        let (lines, _) = t.undo_time().unwrap();
        assert_eq!(lines, vec!["ab"]);
        // g- again -> seq0 ("a")
        let (lines, _) = t.undo_time().unwrap();
        assert_eq!(lines, vec!["a"]);
        // g+ moves forward in time again
        let (lines, _) = t.redo_time().unwrap();
        assert_eq!(lines, vec!["ab"]);
    }
}
