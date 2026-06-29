//! `harness-core` — the project-agnostic data and logic layer behind the
//! `harness` CLI.
//!
//! All run state lives on disk under `.harness/` and `.specs/`; the modules here
//! read, write, and validate that state. The [`snapshot`] module is the
//! UI-agnostic read model the terminal dashboard renders from.

pub mod aclc;
pub mod config;
pub mod hooks;
pub mod layers;
pub mod loop_runner;
pub mod manifest;
pub mod memory;
pub mod oracle;
pub mod prompt;
pub mod scope;
pub mod snapshot;
pub mod spec;
pub mod state;
pub mod util;
