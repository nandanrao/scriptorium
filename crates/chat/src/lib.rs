//! Chat threads for the built-in agent, shared by `scriptorium chat` and the
//! app's agent panel.
//!
//! The [`core`] module is pure (no I/O) and builds without the `shell`
//! feature, so it compiles for wasm.
//!
//! The shell (feature `shell`, on by default) keeps threads in
//! `<script>.threads/` next to the script ([`store`]), locks a thread for the
//! length of a run ([`lock`]), and supplies the clock and thread ids
//! ([`clock`]), deterministic under `SCRIPTORIUM_TEST_CLOCK`. It runs the
//! agent's tool calls through the one apply path both programs share
//! ([`exec`]: the [`exec::ReviewStore`] trait, the thread's own review, the
//! CLI's [`exec::FileStore`]), remembers first-use disclosures ([`seen`]),
//! and runs a thread from the command line ([`host::run`]).

pub mod core;
pub use core::{labels, model, naming};

#[cfg(feature = "shell")]
pub mod clock;
#[cfg(feature = "shell")]
pub mod exec;
#[cfg(feature = "shell")]
pub mod host;
#[cfg(feature = "shell")]
pub mod lock;
#[cfg(feature = "shell")]
pub mod seen;
#[cfg(feature = "shell")]
pub mod store;
