//! Editor state machine: the `Editor` context, windows/splits, modal dispatch,
//! motions, and operators.
//!
//! This crate replaces the "current buffer/window globals + dispatch tables"
//! core of Neovim: `normal.c` (dispatch), `ops.c` (operators), `textobject.c`
//! (motions), `edit.c` (insert mode), the `win_T` model half of `window.c`, and
//! the `state.c` mode loop — all threaded through an explicit [`Editor`] context
//! instead of `curbuf`/`curwin` globals.

pub mod editor;
pub mod input;
pub mod mode;
pub mod motion;
pub mod operator;
pub mod session;
pub mod window;

pub use editor::{BufferState, Editor};
pub use input::Key;
pub use mode::{Mode, VisualKind};
pub use operator::{apply_operator, Operator, OperatorSpan};
pub use session::Session;
pub use window::{Frame, Window};
