//! prx — local-first PR review lifecycle manager.
//!
//! Library surface for the `prx` binary and for integration tests. See
//! `docs/internal/DEV-PLAN.md` for the binding design contract.

pub mod agent;
pub mod autostart;
pub mod browser;
pub mod cli;
pub mod config;
pub mod config_edit;
pub mod daemon;
pub mod discover;
pub mod effects;
pub mod github;
pub mod history;
pub mod insights;
pub mod ipc;
pub mod llm;
pub mod logging;
pub mod machine;
pub mod notify;
pub mod paths;
pub mod pause;
pub mod prompt;
pub mod review;
pub mod state;
pub mod status;
pub mod usage;
