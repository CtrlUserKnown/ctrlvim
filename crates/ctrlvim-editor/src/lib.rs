//! Editor state machine: the `Editor` context, windows/splits, modal dispatch,
//! motions, and operators.
//!
//! This crate replaces the "current buffer/window globals + dispatch tables"
//! core of Neovim: `normal.c` (dispatch), `ops.c` (operators), `textobject.c`
//! (motions), `edit.c` (insert mode), the `win_T` model half of `window.c`, and
//! the `state.c` mode loop — all threaded through an explicit [`Editor`] context
//! instead of `curbuf`/`curwin` globals.

pub mod editor;
pub mod ex;
pub mod fold;
pub mod input;
pub mod jumps;
pub mod keymap;
pub mod mode;
pub mod motion;
pub mod operator;
pub mod pattern;
pub mod quickfix;
pub mod range;
pub mod replace;
pub mod session;
pub mod tags;
pub mod textobject;
pub mod window;

pub use editor::{BufferState, Editor};
pub use fold::{Fold, Folds};
pub use ex::{
    commands as ex_commands, is_ex_command, BufferCmd, ExCommand, ExEffect, QuickfixCmd, TagCmd,
};
pub use input::Key;
pub use keymap::Keymap;
pub use mode::{Mode, Selection, VisualKind};
pub use operator::{apply_operator, Operator, OperatorSpan};
pub use quickfix::{QfItem, QfKind, QuickfixList};
pub use replace::{ReplaceHit, ReplacePlan};
pub use session::Session;
pub use tags::{Tag, TagAddress, TagStack, TagTable};
pub use window::{Frame, Window};
