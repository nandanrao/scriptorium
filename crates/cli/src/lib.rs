//! `scriptorium_cli`: the shell behind the `scriptorium` command line.
//!
//! Everything the app can do is reachable from here, so coding agents can use
//! Scriptorium as fully as people do. This crate is the imperative shell
//! (AGENTS.md rule 1): it reads and writes files around the pure crates
//! (`core`, `fdx`, `review`, `tools`), and does nothing else.
//!
//! - [`opened`]: a script opened with its saved session and review files.
//! - [`fsio`]: atomic writes; every file this crate writes goes through
//!   [`fsio::write_atomic`].
//! - [`commands`]: each command, returning the text it prints.

pub mod commands;
pub mod fsio;
pub mod opened;

pub use commands::{USAGE, run, tool, tool_specs};
pub use fsio::write_atomic;
pub use opened::Opened;
