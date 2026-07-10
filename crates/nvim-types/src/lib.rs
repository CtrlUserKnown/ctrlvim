//! Foundational types shared across the ctrlvim workspace.
//!
//! This is the leaf crate: it has no dependencies on any other workspace crate.
//! It defines the dynamic [`Object`] value type (Neovim's `Object`), arena
//! [`handle`]s that replace raw `curbuf`/`curwin` pointers, editor [`Position`]
//! and [`Range`] types, and the shared [`Error`] type.

pub mod handle;
pub mod object;
pub mod pos;

pub use handle::{BufferId, TabpageId, WindowId};
pub use object::Object;
pub use pos::{Position, Range};

/// Errors surfaced across API boundaries. Mirrors Neovim's `Error` struct,
/// which distinguishes validation errors from generic exceptions.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    /// A caller passed an argument that failed validation (wrong type, out of
    /// range, unknown key). Corresponds to Neovim's `kErrorTypeValidation`.
    #[error("validation: {0}")]
    Validation(String),
    /// A runtime failure while executing an otherwise well-formed request.
    /// Corresponds to Neovim's `kErrorTypeException`.
    #[error("exception: {0}")]
    Exception(String),
}

impl Error {
    pub fn validation(msg: impl Into<String>) -> Self {
        Error::Validation(msg.into())
    }
    pub fn exception(msg: impl Into<String>) -> Self {
        Error::Exception(msg.into())
    }
}

/// The canonical result type for API-surface calls.
pub type Result<T> = std::result::Result<T, Error>;
