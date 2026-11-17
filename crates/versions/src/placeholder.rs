//! The placeholder version (`version new`): the original's structure with
//! the text to translate blanked, every line linked 1:1. Plan:
//! `docs/plans/versions.md`, step A3.
//!
//! Per paragraph of the original's *file* script (C8):
//!
//! - empty (nothing but whitespace) → kept, `=` (nothing to translate; an
//!   empty heading would otherwise read "not yet translated" forever);
//! - Scene Heading → kept, `~` its text (it gets a suggestion);
//! - Character → kept, `=` (cues count as in sync);
//! - non-editable kinds (`ElementKind::Other`: "End of Act", …) → kept, `=`;
//! - everything else → text emptied, `~ ""`.
//!
//! The original's side is stamped `=` its text throughout. Blanking is a
//! text edit only: ids, kinds, `fixed` (dual dialogue) and `id_floor` are the
//! original's, so the host writes the result with `FdxFile::write_script` on
//! the original's `FdxFile` and every byte outside the blanked runs is kept.

use std::collections::BTreeMap;
use std::fmt;

use scriptorium_core::{ElementKind, ParaId, Paragraph, Script, Span};

use crate::links::{Links, Member, Para, Row, Stamp, stamp_text};

/// The `created:` value of a version made by `version new`.
pub const CREATED_BY_VERSION_NEW: &str = "version new";

/// Why `blank_copy` can't make a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlankCopyError {
    /// The script has no paragraphs: nothing to line up.
    Empty,
    /// The script isn't the original's file as read (`FdxFile::to_script`):
    /// paragraph `index` doesn't have id `index`. The links file names only
    /// file ids, so a live session's script can't be used.
    NotFileIds { index: usize, id: ParaId },
}

impl fmt::Display for BlankCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlankCopyError::Empty => f.write_str("the script has no lines to make a version of"),
            BlankCopyError::NotFileIds { index, id } => write!(
                f,
                "line {} has id {} instead of its place in the file; a version is made from the script's file as read",
                index + 1,
                id.0
            ),
        }
    }
}

impl std::error::Error for BlankCopyError {}

/// The placeholder version of `original` (the original's file script, ids
/// `0..n` in order) and the links file lining them up row `i | i`.
///
/// `id` is the links id; `original_member` and `version_member` become
/// members `a` and `b`. Their `created` is overwritten on both: the
/// version's becomes [`CREATED_BY_VERSION_NEW`], the original's `None`.
/// `is_caps` says which kinds are shown in capitals (callers pass
/// `|k| f.is_caps(k.name())`).
///
/// The host makes the file's bytes: `write_script` of the returned script on
/// the original's `FdxFile`, then `scriptorium_fdx::clear_text_offsets` (the
/// original's script notes and spell-check ranges are character offsets
/// into the English text, wrong for the version, so they are emptied). The
/// version's hash can't be known before that: the host puts the hash of
/// those bytes in `links.members[1].hash` before writing the links file.
pub fn blank_copy(
    original: &Script,
    id: &str,
    original_member: Member,
    version_member: Member,
    is_caps: impl Fn(&ElementKind) -> bool,
) -> Result<(Script, Links), BlankCopyError> {
    if original.paragraphs.is_empty() {
        return Err(BlankCopyError::Empty);
    }
    if let Some((index, p)) = original.paragraphs.iter().enumerate().find(|(i, p)| p.id != ParaId(*i as u64)) {
        return Err(BlankCopyError::NotFileIds { index, id: p.id });
    }

    let mut version = original.clone();
    let mut rows = Vec::with_capacity(original.paragraphs.len());
    let mut a_paras = BTreeMap::new();
    let mut b_paras = BTreeMap::new();
    for (orig, copy) in original.paragraphs.iter().zip(version.paragraphs.iter_mut()) {
        let text = stamp_text(&orig.text(), is_caps(&orig.kind));
        let b_stamp = if text.is_empty() || matches!(orig.kind, ElementKind::Character | ElementKind::Other(_)) {
            Stamp::Synced(text.clone())
        } else if orig.kind == ElementKind::SceneHeading {
            Stamp::Placeholder(text.clone())
        } else {
            blank(copy);
            Stamp::Placeholder(String::new())
        };
        rows.push(Row { cells: vec![vec![orig.id], vec![orig.id]] });
        a_paras.insert(orig.id, Para { kind: orig.kind.clone(), stamp: Some(Stamp::Synced(text)) });
        b_paras.insert(orig.id, Para { kind: orig.kind.clone(), stamp: Some(b_stamp) });
    }

    let links = Links {
        id: id.to_string(),
        members: vec![
            Member { created: None, ..original_member },
            Member { created: Some(CREATED_BY_VERSION_NEW.to_string()), ..version_member },
        ],
        rows,
        paras: vec![a_paras, b_paras],
        extra_headers: Vec::new(),
    };
    Ok((version, links))
}

/// Empty a paragraph's text, keeping the first span's style: what the file
/// reads back as once the writer has emptied the first run (it keeps that
/// run's attributes, `fdx::restyle`).
fn blank(p: &mut Paragraph) {
    let style = p.spans.first().map(|s| s.style).unwrap_or_default();
    p.spans = vec![Span { text: String::new(), style }];
}
