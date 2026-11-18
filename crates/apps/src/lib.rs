//! Apps in the writer's browser: small web apps an agent builds in the
//! script's folder (`apps/<name>/`), served by the running Scriptorium on
//! `127.0.0.1:28417` (see `docs/plans/apps.md`).
//!
//! This crate's modules are pure (AGENTS.md rule 1): they take and return
//! values, never touch files, sockets or clocks, and compile to
//! `wasm32-unknown-unknown`. Time comes in as milliseconds from the caller.
//!
//! - [`manifest`]: `app.json` and the app-name rule.
//! - [`paths`]: the project key, and request paths split into checked parts.
//! - [`route`]: what to do with a request ([`route::decide`]), the response
//!   headers, MIME types, the cookie, and the server's own pages.
//! - [`events`]: change events from snapshots, their pacing, and SSE frames.
//! - [`list`]: the apps list as text (agents, CLI) and JSON.
//! - [`LIBRARY`]: `/scriptorium.js`, served to every app.

pub mod events;
pub mod list;
pub mod manifest;
pub mod paths;
pub mod route;

/// The one port apps are served on (decision 3: fixed, no fallback).
pub const PORT: u16 = 28417;

/// The library every app loads from `/scriptorium.js`.
pub const LIBRARY: &str = include_str!("../assets/scriptorium.js");

/// This build's version, sent in `/api/status` and the `hello` event.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The largest request body for `call` and `log` (a 200-item
/// `suggest_changes` fits).
pub const BODY_MAX: u64 = 1024 * 1024;
/// The largest data file an app may write.
pub const DATA_MAX: u64 = 20 * 1024 * 1024;
/// The most header bytes a request may send (`tiny_http` has no limit).
pub const HEADERS_MAX: usize = 16 * 1024;
/// The most event listeners at once; one more is answered 503.
pub const LISTENERS_MAX: usize = 32;
