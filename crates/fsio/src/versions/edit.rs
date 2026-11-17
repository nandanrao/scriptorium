//! Changing a pair's links file row by row (plan step B3): Mark in sync,
//! Mark not in sync and join, for the app's Edit menu and the CLI's `links`
//! command, through one call ([`edit_row`]). The work is
//! `scriptorium_versions::edit`; this checks that the links file and both
//! files are still the ones the pair was loaded from (C6), and writes the
//! links file under its lock. Nothing else is written, and nothing goes on
//! an undo stack.

use std::time::Duration;

use scriptorium_core::{Document, ElementKind, Script};
use scriptorium_fdx::FdxFile;
use scriptorium_review::Session;
use scriptorium_review::sessionfile;
use scriptorium_versions::links;
use scriptorium_versions::{RowStatus, Toward, join_rows, lay_out, sync_row, unsync_row};

use super::pair::{LoadedPair, lock_links_within};
use crate::write_atomic_no_link;

/// What to do to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowEdit {
    /// `=` stamps with both sides' text now.
    Sync,
    /// The original's `=` stamps taken away (`versions::unsync_row`).
    Unsync,
    /// A row with nothing on one side joined with a neighbour: the one
    /// given, or the one that fills the empty side (`versions::join_rows`).
    Join(Option<Toward>),
}

/// What [`edit_row`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowEdited {
    /// The row, numbered from 1 as `rows` numbers it: for a join, the
    /// joined row.
    pub row: usize,
    /// The row's status afterwards, from the version's side.
    pub status: RowStatus,
    /// For a join: the neighbour joined with (numbered as before the join).
    pub joined: Option<usize>,
}

/// Change row `row` (numbered from 1) of `pair`, loaded for the script
/// `this` (its file as opened) with its `session` and live `doc` (the
/// rows read the version's live text and the original's file, as
/// everywhere). Waits up to `wait` for the links lock (the app: none).
///
/// Refused when the links file changed since the pair was loaded (load it
/// again first), when either file isn't the bytes the links file describes
/// (C6), or as `scriptorium_versions::edit` refuses. The caller words the
/// result; `notes` takes what the lock says on a file system without locks.
#[allow(clippy::too_many_arguments)]
pub fn edit_row(
    pair: &LoadedPair,
    this: &FdxFile,
    session: &Session,
    doc: &Document,
    row: usize,
    edit: RowEdit,
    wait: Duration,
    notes: &mut Vec<String>,
) -> Result<RowEdited, String> {
    let at = &pair.path;
    let links_name = crate::opened::file_name(at);
    let index = row.checked_sub(1).ok_or("rows are numbered from 1")?;
    let _guard = lock_links_within(at, wait, notes)?;
    let text = std::fs::read_to_string(at).map_err(|e| format!("{}: {e}", at.display()))?;
    if sessionfile::hash(&text) != pair.links_hash {
        return Err(format!("{links_name} changed since it was read; try again in a moment"));
    }
    // Both files as they are on disk now, under the lock (C6): this one's
    // bytes as opened, the other's as loaded.
    let this_file = &pair.links.members[pair.this].file;
    let this_path = at.with_file_name(this_file);
    let on_disk = std::fs::read_to_string(&this_path).map_err(|e| format!("{}: {e}", this_path.display()))?;
    let this_hash = sessionfile::hash(this.raw());
    if pair.links.members[pair.this].hash != this_hash || sessionfile::hash(&on_disk) != this_hash {
        return Err(format!("{this_file} changed outside Scriptorium; reopen it"));
    }
    let other = std::fs::read_to_string(&pair.other_path).map_err(|e| format!("{}: {e}", pair.other_path.display()))?;
    if sessionfile::hash(&other) != pair.other_hash {
        let other_file = &pair.links.members[pair.other_index()].file;
        return Err(format!("{other_file} changed outside Scriptorium; reopen it"));
    }

    let laid = pair.lay_out(this, session, doc);
    let scripts: Vec<&Script> = laid.scripts.iter().map(|s| s.as_ref()).collect();
    // One `is_caps` for both members, the original's (plan, "Status").
    let original: &FdxFile = if pair.this_is_version() { &pair.other } else { this };
    let is_caps = |k: &ElementKind| original.is_caps(k.name());
    let why = |e: scriptorium_versions::EditError| format!("Row {row}: {e}");
    let (out, at_row) = match edit {
        RowEdit::Sync => {
            let live = laid.rows.get(index).map_or(&[][..], |r| r.cells.as_slice());
            (sync_row(&pair.links, index, live, &scripts, &is_caps).map_err(why)?, index)
        }
        RowEdit::Unsync => (unsync_row(&pair.links, index).map_err(why)?, index),
        RowEdit::Join(toward) => join_rows(&pair.links, index, toward).map_err(why)?,
    };
    let written = links::write(&out).map_err(|e| format!("{links_name}: {e}"))?;
    write_atomic_no_link(at, written.as_bytes()).map_err(|e| format!("{}: {e}", at.display()))?;

    // The row as it reads now, with the suggestions placed as before.
    let (o, v) = out.sides();
    let placed: Vec<_> = if pair.this_is_version() {
        super::placed_items(session, doc, &out.id).into_iter().map(|p| p.placed).collect()
    } else {
        vec![]
    };
    let rows = lay_out(&out, scripts[o], scripts[v], &placed, &is_caps);
    let status = rows.get(at_row).map_or(RowStatus::OneSided, |r| r.status);
    let joined = matches!(edit, RowEdit::Join(_)).then(|| if at_row < index { at_row + 1 } else { index + 2 });
    Ok(RowEdited { row: at_row + 1, status, joined })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::versions::{PairState, resume, version_new};

    const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Laura smiles.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

    /// `film.fdx` and its version in a fresh folder; the version opened.
    fn setup(name: &str) -> (std::path::PathBuf, FdxFile, scriptorium_review::sessionfile::ResumedSession, PairState) {
        let dir = std::env::temp_dir().join(format!("scriptorium-fsio-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        version_new(&dir.join("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
        let fdx = FdxFile::parse(std::fs::read_to_string(dir.join("film.es.fdx")).unwrap()).unwrap();
        let (o, pair) = resume(&fdx, &dir, "film.es.fdx", vec![], None, &mut vec![]);
        (dir, fdx, o, pair)
    }

    fn edit(
        pair: &PairState,
        fdx: &FdxFile,
        o: &scriptorium_review::sessionfile::ResumedSession,
    ) -> Result<RowEdited, String> {
        let p = pair.loaded().unwrap();
        edit_row(p, fdx, &o.session, &o.doc, 1, RowEdit::Sync, Duration::ZERO, &mut vec![])
    }

    #[test]
    fn a_links_file_changed_since_it_was_read_is_refused() {
        let (dir, fdx, o, pair) = setup("stale");
        let at = dir.join("film.es.links.txt");
        let text = std::fs::read_to_string(&at).unwrap();
        std::fs::write(&at, format!("{text}# a comment\n")).unwrap();
        let e = edit(&pair, &fdx, &o).unwrap_err();
        assert!(e.contains("film.es.links.txt changed since it was read"), "{e}");
        assert!(std::fs::read_to_string(&at).unwrap().ends_with("# a comment\n"), "not written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn either_file_changed_on_disk_is_refused() {
        let (dir, fdx, o, pair) = setup("changed");
        let before = std::fs::read_to_string(dir.join("film.es.links.txt")).unwrap();
        let en = dir.join("film.fdx");
        std::fs::write(&en, SCRIPT.replace("Laura smiles.", "Laura laughs.")).unwrap();
        let e = edit(&pair, &fdx, &o).unwrap_err();
        assert_eq!(e, "film.fdx changed outside Scriptorium; reopen it");
        std::fs::write(&en, SCRIPT).unwrap();
        assert!(edit(&pair, &fdx, &o).is_ok(), "the same bytes again");
        // This file saved by Final Draft since it was opened.
        let (dir2, fdx2, o2, pair2) = setup("changed-this");
        let es = dir2.join("film.es.fdx");
        std::fs::write(&es, format!("{}\n", fdx2.raw())).unwrap();
        let e = edit(&pair2, &fdx2, &o2).unwrap_err();
        assert_eq!(e, "film.es.fdx changed outside Scriptorium; reopen it");
        assert_ne!(std::fs::read_to_string(dir.join("film.es.links.txt")).unwrap(), before, "the good one wrote");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
