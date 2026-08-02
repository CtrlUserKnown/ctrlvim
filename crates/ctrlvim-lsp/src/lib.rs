//! A minimal LSP client: enough of the protocol to run a language server as
//! a subprocess, keep it in sync with an open buffer, and ask it for
//! completions. Not a general-purpose LSP library — see `client.rs`'s module
//! docs for exactly what's in and out of scope.

mod client;
mod codec;

pub use client::{uri_from_path, ClientStatus, CompletionItem, LspClient, LspEvent};
