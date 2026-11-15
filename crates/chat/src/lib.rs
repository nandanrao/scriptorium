//! Chat threads for the built-in agent, shared by `scriptorium chat` and the
//! app's agent panel.
//!
//! The [`core`] module is pure (no I/O) and builds without the `shell`
//! feature, so it compiles for wasm.
//!
//! The shell (feature `shell`, on by default) keeps threads in
//! `<script>.threads/` next to the script ([`store`]), locks a thread for the
//! length of a run ([`lock`]), and supplies the clock and thread ids
//! ([`clock`]), deterministic under `SCRIPTORIUM_TEST_CLOCK`.

pub mod core;
pub use core::{labels, model, naming};

#[cfg(feature = "shell")]
pub mod clock;
#[cfg(feature = "shell")]
pub mod lock;
#[cfg(feature = "shell")]
pub mod store;
