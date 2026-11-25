//! gpufree — check GPU availability across SSH hosts.
//!
//! The binary is a thin wrapper around this library so that the pure logic
//! (config, parsing, state, filtering, TUI state) is unit-testable without a
//! terminal or a real sshd.

pub mod cli;
pub mod completion;
pub mod config;
pub mod doctor;
pub mod model;
pub mod output;
pub mod probe;
pub mod run;
pub mod signal;
pub mod tui;
