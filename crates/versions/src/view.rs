//! A pair's rows as a host laid them out for one of its scripts, handed to
//! the tools as one value ([`PairView`]): the shell (`scriptorium_fsio`)
//! makes it from the pair it loaded, the tools (`scriptorium_tools::LinksCtx`)
//! read it. Plain data, so every host (the CLI, MCP, the chat agent, the app
//! over its own session) builds it with the same call.

use std::borrow::Cow;
use std::path::Path;

use scriptorium_core::Script;

use crate::links::Links;
use crate::rows::LaidRow;

/// A pair's rows as laid out for the script `this` (`rows::lay_out`).
#[derive(Debug, Clone)]
pub struct PairView<'a> {
    pub links: &'a Links,
    /// The links file, absolute; both members are in its folder.
    pub path: &'a Path,
    /// The member the rows are shown on (the script the tools are called on).
    pub this: usize,
    /// The rows, in the links file's order (numbered from 1).
    pub rows: Vec<LaidRow>,
    /// Each member's script as the rows read it (indexed by member).
    pub scripts: Vec<Cow<'a, Script>>,
    /// The (review, item) in the version's session of each suggestion the
    /// rows index ([`LaidRow::suggestions`]); empty on the original.
    pub placed: Vec<(usize, usize)>,
}
