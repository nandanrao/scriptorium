//! `scriptorium_cli`: the shell behind the `scriptorium` command line.
//!
//! Everything the app can do is reachable from here, so coding agents can use
//! Scriptorium as fully as people do. This crate is the imperative shell
//! (AGENTS.md rule 1): it reads and writes files around the pure crates
//! (`core`, `fdx`, `review`, `tools`), and does nothing else.
//!
//! - [`opened`]: a script opened with its saved session and review files.
//! - [`fsio`] (the `scriptorium-fsio` crate): atomic writes; every file this crate writes goes through
//!   [`fsio::write_atomic`]. Every command that writes holds the script's
//!   [`fsio::SessionLock`] from opening the session to saving it.
//! - [`structure`]: insert, remove, split, join and retype, as typed functions.
//! - [`commands`]: each command, returning the text it prints.

pub mod commands;
/// Moved to the `scriptorium-fsio` crate; re-exported so `scriptorium_cli::fsio`
/// paths keep working. New code depends on `scriptorium-fsio` directly.
pub use scriptorium_fsio as fsio;
pub mod opened;
pub mod structure;

pub use commands::{Choice, SuggestionRef, USAGE, run, tool, tool_specs};
pub use fsio::{SessionLock, write_atomic};
pub use opened::Opened;
