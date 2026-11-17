//! Versions: two (later N) `.fdx` files of one script lined up row by row —
//! a translation, an older draft, a co-writer's copy.
//!
//! Rules for this crate (see AGENTS.md): pure, no file or network I/O, no
//! threads, no clocks, no UI. It compiles to `wasm32-unknown-unknown`. The
//! shell (finding the pair on disk, locks, writes) is
//! `scriptorium_fsio::versions`. The plan is `docs/plans/versions.md`.

pub mod align;
pub mod links;
pub mod placeholder;
pub mod resolve;
pub mod rows;

pub use align::{Alignment, by_text, refind};
pub use links::{Links, LinksError, Member, Para, Row, Stamp, language_name, stamp_text};
pub use placeholder::{BlankCopyError, blank_copy};
pub use resolve::{NotFound, Resolver, RowQuery};
pub use rows::{LaidRow, Note, Placed, PlacedState, RowStatus, lay_out};
