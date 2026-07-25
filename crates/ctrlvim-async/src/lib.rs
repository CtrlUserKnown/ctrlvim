//! Async I/O layer: the tokio-backed replacement for `event/*.c` (libuv loop),
//! the msgpack-RPC channel, and the substrate for `vim.uv`.
//!
//! * [`event::EventLoop`] — the multiqueue that marshals background events onto
//!   the main editor thread.
//! * [`timer::TimerService`] — tokio timers feeding the queue (the base of
//!   `vim.uv.new_timer`).
//! * [`job::Jobs`] — spawned processes streaming output onto the queue (the
//!   base of `:make`/`:grep` and, later, LSP server channels).
//! * [`rpc`] — msgpack-RPC encode/decode/dispatch on `rmpv`.
//!
//! The Lua-facing `vim.uv` binding itself lives in `ctrlvim-lua` (which depends on
//! this crate) so callback invocation stays on the single-threaded editor side.

pub mod event;
pub mod job;
pub mod rpc;
pub mod timer;

pub use event::{Event, EventLoop};
pub use job::{Jobs, LineBuffer};
pub use timer::{TimerHandle, TimerService};
