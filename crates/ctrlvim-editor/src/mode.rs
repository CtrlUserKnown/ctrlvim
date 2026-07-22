//! Editor modes — the `Mode` model replacing Neovim's `State` bitmask and the
//! generic `VimState{check,execute}` interface (`state.c`).
//!
//! Neovim reuses one `check`/`execute` function-pointer pair across Normal,
//! Insert, Cmdline, and Terminal modes. We express the same shape as an `enum`
//! plus a dispatch function per mode (see `normal.rs`, `insert.rs`), which keeps
//! the state machine explicit and avoids the scattered globals (`VIsual_active`,
//! `restart_edit`, `finish_op`).

use ctrlvim_types::Position;

/// Visual sub-mode. `Select` wraps the underlying visual kind (Neovim's
/// Select-mode is Visual-mode that replaces the selection on a printable key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualKind {
    Char,
    Line,
    Block,
}

/// The current editor mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    /// Visual mode with an anchor position and a kind.
    Visual { anchor: Position, kind: VisualKind },
    /// Command-line mode (`:`), accumulating the typed command.
    Cmdline { prefix: char, buffer: String },
}

impl Mode {
    /// Short name as reported by `ctrlvim_get_mode()` / `mode()`.
    pub fn short_name(&self) -> &'static str {
        match self {
            Mode::Normal => "n",
            Mode::Insert => "i",
            Mode::Visual { kind: VisualKind::Char, .. } => "v",
            Mode::Visual { kind: VisualKind::Line, .. } => "V",
            Mode::Visual { kind: VisualKind::Block, .. } => "\u{16}", // CTRL-V
            Mode::Cmdline { .. } => "c",
        }
    }

    pub fn is_visual(&self) -> bool {
        matches!(self, Mode::Visual { .. })
    }
}

/// A normalized visual selection, for a frontend to highlight. `start <= end`
/// in `(line, col)` order regardless of which way the selection was dragged.
///
/// The `col` fields carry the anchor/cursor columns as-is; interpreting them
/// per [`VisualKind`] (charwise flows across line ends, linewise ignores
/// columns, blockwise takes the min/max column band) is the renderer's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub kind: VisualKind,
    pub start: Position,
    pub end: Position,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_short_names() {
        assert_eq!(Mode::Normal.short_name(), "n");
        assert_eq!(Mode::Insert.short_name(), "i");
        assert_eq!(
            Mode::Visual { anchor: Position::default(), kind: VisualKind::Line }.short_name(),
            "V"
        );
    }
}
