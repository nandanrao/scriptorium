//! Chat threads for the built-in agent, shared by `scriptorium chat` and the
//! app's agent panel.
//!
//! The shell (feature `shell`, on by default) keeps threads in
//! `<script>.threads/` next to the script ([`store`]), locks a thread for the
//! length of a run ([`lock`]), and supplies the clock and thread ids
//! ([`clock`]), deterministic under `SCRIPTORIUM_TEST_CLOCK`.

#[cfg(feature = "shell")]
pub mod clock;
#[cfg(feature = "shell")]
pub mod lock;
#[cfg(feature = "shell")]
pub mod store;
