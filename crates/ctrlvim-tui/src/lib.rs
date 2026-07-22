//! ctrlvim's Ratatui frontend, exposed as a library so the binary and the
//! integration tests share the same modules.
//!
//! The UI is a faithful port of the "Charvim Dashboard" design: a startup
//! dashboard (workspace / settings / about), a plugin manager, a file-buffer
//! viewer, and floating explorer / command-palette / help overlays, all driven
//! by both keyboard and mouse. [`app::App`] owns a real [`ctrlvim_core::Ctrlvim`]
//! engine so opening a file loads its contents through the backend.

pub mod app;
pub mod config;
pub mod data;
pub mod input;
pub mod model;
pub mod theme;
pub mod ui;
