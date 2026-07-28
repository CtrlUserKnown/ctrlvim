//! Buffer text engine: rope-backed storage, unified marks, undo tree, registers.
//!
//! This crate replaces Neovim's `memline.c` (buffer storage), `mark.c` +
//! `marktree.c` + `extmark.c` (marks), `undo.c` (undo tree), and `register.c`
//! (registers) with idiomatic Rust equivalents. It has no notion of modes,
//! windows, or Lua — it is the pure text/data layer everything else builds on.

pub mod buffer;
pub mod marks;
pub mod width;
pub mod register;
pub mod undo;

pub use buffer::Buffer;
pub use marks::{Gravity, MarkStore, Namespace, NS_LEGACY_MARKS};
pub use width::{char_index_at, char_width, display_width, width_upto};
pub use register::{MotionType, Registers, YankReg};
pub use undo::UndoTree;
