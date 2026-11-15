//! Moved to `scriptorium_fsio::opened` (feature `opened`), so the chat host
//! can open a script the way the CLI does without depending on this crate;
//! re-exported so `scriptorium_cli::opened` paths keep working.

pub use scriptorium_fsio::opened::*;
