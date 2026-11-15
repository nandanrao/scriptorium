//! The heart of Scriptorium: the screenplay document model and the pure helpers
//! every other crate builds on.
//!
//! Rules for this crate (see AGENTS.md): no file or network I/O, no threads, no
//! UI. It must compile to `wasm32-unknown-unknown` so the same code runs in the
//! desktop app and in the browser.

pub mod diff;
pub mod doc;
pub mod model;
pub mod outline;
pub mod paste;
pub mod rules;
pub mod spans;
pub mod text;

pub use doc::{DocError, Document, Edit, FIXED_MESSAGE, Group, NewPara, Pos, Replaced, Run, Selection};
pub use model::{ElementKind, ParaId, Paragraph, Script, Span, Style};
