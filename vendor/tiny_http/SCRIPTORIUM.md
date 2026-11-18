# tiny_http 0.12.0, vendored for Scriptorium

Copied unchanged from crates.io `tiny_http` 0.12.0 (MIT OR Apache-2.0; see
`LICENSE-MIT` and `LICENSE-APACHE`), used through `[patch.crates-io]` in the
workspace `Cargo.toml`, with one change.

## The change

`src/util/equal_reader.rs`, `impl Drop for EqualReader`: the drain of an
unread request body reads into a fixed 8 KB buffer until the announced
length is read, the stream ends, or a read fails. Upstream allocated one
buffer as large as the remaining announced `Content-Length`
(`vec![0; remaining_to_read]`).

## Why

The apps server (`crates/apps/src/serve.rs`) refuses an over-long body
(413) without reading it, so the request's body reader is dropped unread.
With upstream's drain, a request announcing a huge `Content-Length` made
that allocation fail and aborted the whole Scriptorium process, whatever
the server's checks decided: anything that can reach 127.0.0.1 could
crash it.

When tiny_http publishes a release with this fixed, drop the vendored copy
and the patch.

## Also

`Cargo.toml` gains a `[lints]` table at its end that allows upstream's
`unused` warnings and every clippy lint, so the workspace's
`cargo clippy --all-targets` stays warning-free. The crate is excluded from
the workspace (root `Cargo.toml`), so `cargo fmt --all` leaves it alone.
Removed from the copy: `.cargo-ok` and `.github/`.
