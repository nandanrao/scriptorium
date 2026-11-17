//! Reviewing proposed changes to a script, one at a time.
//!
//! A *review* is a list of suggestions read from a review file (the plain-text
//! `SCRIPT REVIEW v1` format, see [`file`]). Each suggestion is found in the
//! script by its `BEFORE` text ([`matching`]) and offers one or more
//! replacement texts. A [`Session`] holds every review opened on one script.
//!
//! Decisions are not stored here. Accepting, keeping or rewriting goes through
//! [`scriptorium_core::Document`]: the text change and the decision (kept as
//! document metadata) are one transaction, so typing and reviewing share one
//! undo history, and the document's event log is the record of everything.
//!
//! Pure: strings and values in, values out. Reading and writing files is the
//! caller's job.

pub mod file;
pub mod matching;
mod meta;
mod session;
pub mod sessionfile;

pub use file::{Entry, FileStatus, Line, ReviewFile, RowRef};
pub use matching::{NO_LINKS, RowResolver};
pub use session::{
    Appended, AppendedEntry, AppendedMany, CurrentRun, Decision, Item, LineChange, Progress, Refusal, Review, Session,
    Shape, Status, Unmatched, Unplaced,
};
