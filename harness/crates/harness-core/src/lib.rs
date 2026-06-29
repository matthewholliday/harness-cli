//! `harness-core` — the project-agnostic data and logic layer shared by every
//! Harness front-end.
//!
//! The CLI (`harness-cli`) and the desktop GUI (`harness-gui`) both depend on
//! this crate. All run state lives on disk under `.harness/` and `.specs/`; the
//! modules here read, write, and validate that state. The [`snapshot`] module is
//! the UI-agnostic read model both front-ends render from.

pub mod config;
pub mod hooks;
pub mod loop_runner;
pub mod manifest;
pub mod prompt;
pub mod snapshot;
pub mod spec;
pub mod state;
pub mod util;
