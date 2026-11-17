//! `scriptorium links <script.fdx> sync|unsync <row>` and `join <row>
//! [up|down]` (plan step B3): the app's Edit › Mark in sync and Mark not in
//! sync, and joining a row a merge left with nothing on one side with a
//! neighbour. Rows are numbered as
//! `rows` numbers them. Only the links file is written (under its lock);
//! the script's session is read, never saved, and nothing goes on the undo
//! stack. The work is [`scriptorium_fsio::versions::edit_row`], which the
//! app calls too.

use scriptorium_fsio::LOCK_WAIT;
use scriptorium_fsio::versions::{RowEdit, edit_row};
use scriptorium_tools::LinksCtx;
use scriptorium_versions::Toward;

/// The commands this file handles, dispatched from `commands::run_other`.
pub const COMMANDS: &[&str] = &["links"];

pub fn command(cmd: &str, args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (script, edit, row) = match args {
        [script, "sync", row] => (*script, RowEdit::Sync, *row),
        [script, "unsync", row] => (*script, RowEdit::Unsync, *row),
        [script, "join", row] => (*script, RowEdit::Join(None), *row),
        [script, "join", row, "up"] => (*script, RowEdit::Join(Some(Toward::Up)), *row),
        [script, "join", row, "down"] => (*script, RowEdit::Join(Some(Toward::Down)), *row),
        _ => return Err(format!("wrong arguments for `{cmd}`\n\n{}", crate::USAGE)),
    };
    let n = row.trim_start_matches('#').parse::<usize>().map_err(|_| format!("row must be a number, got {row:?}"))?;
    let o = crate::rows::open(script, notes)?.ok_or_else(|| crate::rows::no_pair(script))?;
    let pair = o.pair().loaded().expect("opened with a pair");
    let done = edit_row(pair, &o.fdx, &o.session, &o.doc, n, edit, LOCK_WAIT, notes)?;
    let label = LinksCtx::from(pair.view(&o.fdx, &o.session, &o.doc)).status_label(done.status);
    let file = scriptorium_fsio::opened::file_name(&pair.path);
    Ok(match edit {
        RowEdit::Sync => format!(
            "marked row {} in sync in {file} (`scriptorium links {script:?} unsync {}` takes it back)\n",
            done.row, done.row
        ),
        RowEdit::Unsync if label == "in sync" => format!(
            "marked row {} not in sync in {file}; it still reads in sync (a decided suggestion, or both sides \
             blank)\n",
            done.row
        ),
        RowEdit::Unsync => format!("marked row {} not in sync in {file}: it reads {label}\n", done.row),
        RowEdit::Join(_) => {
            let other = done.joined.unwrap_or(n);
            format!(
                "joined rows {} and {} into row {} in {file}: it reads {label}\n",
                n.min(other),
                n.max(other),
                done.row
            )
        }
    })
}
