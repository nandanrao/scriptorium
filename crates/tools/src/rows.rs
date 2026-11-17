//! Rows of a pair of versions as agents and the CLI see them: one shape,
//! [`RowView`], for the CLI's `rows --json` and the `read_rows` tool. Pure:
//! the host lays the rows out (`scriptorium_fsio::versions::LoadedPair::lay_out`)
//! and hands them over as a [`LinksCtx`].
//!
//! "This" is the script the rows are shown on, "other" the file lined up
//! with it. [`RowStatus`] is always from the version's side; shown on the
//! original it is swapped for this/other, and no row needs work there
//! (only the version is written).

use std::collections::HashMap;

use scriptorium_core::{Document, ParaId, Paragraph, Script};
use scriptorium_review::Session;
use scriptorium_versions::{LaidRow, RowStatus};
use serde_json::{Value, json};

use crate::ToolError;
use crate::reviews::item_numbers;

/// A pair's rows as the host laid them out, for the script the tools are
/// called on.
#[derive(Debug, Clone)]
pub struct LinksCtx {
    /// The rows, in the links file's order (numbered from 1).
    pub rows: Vec<LaidRow>,
    /// Each member's script as the rows read it (indexed by member): the
    /// version's live script, the original's file.
    pub scripts: Vec<Script>,
    /// Each member's label: its language's name, or its file stem.
    pub labels: Vec<String>,
    /// The member the tools are called on.
    pub this: usize,
    /// The version's member (the made one, or `b`).
    pub version: usize,
    /// Both members name the same language: a blank version line reads
    /// "blank", not "not yet translated".
    pub same_language: bool,
    /// The (review, item) in the session of each suggestion placed on a row,
    /// as [`LaidRow::suggestions`] indexes them. Empty on the original.
    pub placed: Vec<(usize, usize)>,
}

/// One row as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowView {
    /// Its number, from 1.
    pub row: usize,
    /// `in_sync`, `this_changed`, `other_changed`, `both_changed`, `blank`,
    /// `one_sided`, from the script the rows are shown on.
    pub status: &'static str,
    /// The status in words, as the app shows it ("Español changed").
    pub label: String,
    pub needs: bool,
    pub note: Option<&'static str>,
    pub this: Vec<ParaView>,
    pub other: Vec<ParaView>,
    /// The suggestions placed on the row (an old decided one and a new
    /// pending one from an updates review may share it).
    pub suggestions: Vec<SuggestionView>,
}

/// A paragraph of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParaView {
    pub id: u64,
    pub kind: String,
    pub text: String,
}

/// A suggestion on a row: its review's name, its number there (as
/// `list_suggestions` numbers it) and its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionView {
    pub review: String,
    pub n: usize,
    pub status: String,
}

impl RowView {
    /// The row as JSON: `{row, status, label, needs, note?, this: [{id, kind,
    /// text}], other: [...], suggestions: [{review, n, status}]}`.
    pub fn to_json(&self) -> Value {
        let paras = |v: &[ParaView]| -> Vec<Value> {
            v.iter().map(|p| json!({ "id": p.id, "kind": p.kind, "text": p.text })).collect()
        };
        let mut row = json!({
            "row": self.row,
            "status": self.status,
            "label": self.label,
            "needs": self.needs,
            "this": paras(&self.this),
            "other": paras(&self.other),
            "suggestions": self
                .suggestions
                .iter()
                .map(|s| json!({ "review": s.review, "n": s.n, "status": s.status }))
                .collect::<Vec<_>>(),
        });
        if let Some(note) = self.note {
            row["note"] = json!(note);
        }
        row
    }
}

impl LinksCtx {
    /// Whether the rows are shown on the version.
    pub fn on_version(&self) -> bool {
        self.this == self.version
    }

    /// The member lined up with this one.
    pub fn other(&self) -> usize {
        if self.this == 0 { 1 } else { 0 }
    }

    /// The original's member.
    pub fn original(&self) -> usize {
        if self.version == 0 { 1 } else { 0 }
    }

    /// The status as `this`/`other` words, from the script shown on.
    pub fn status_key(&self, status: RowStatus) -> &'static str {
        match (status, self.on_version()) {
            (RowStatus::InSync, _) => "in_sync",
            (RowStatus::ThisChanged, true) | (RowStatus::OtherChanged, false) => "this_changed",
            (RowStatus::OtherChanged, true) | (RowStatus::ThisChanged, false) => "other_changed",
            (RowStatus::BothChanged, _) => "both_changed",
            (RowStatus::Blank, _) => "blank",
            (RowStatus::OneSided, _) => "one_sided",
        }
    }

    /// The status as the app labels it: "in sync", "Español changed",
    /// "English changed", "both changed", "not yet translated" (between two
    /// files in one language: "blank"), "nothing on this side".
    pub fn status_label(&self, status: RowStatus) -> String {
        match status {
            RowStatus::InSync => "in sync".into(),
            RowStatus::ThisChanged => format!("{} changed", self.labels[self.version]),
            RowStatus::OtherChanged => format!("{} changed", self.labels[self.original()]),
            RowStatus::BothChanged => "both changed".into(),
            RowStatus::Blank if self.same_language => "blank".into(),
            RowStatus::Blank => "not yet translated".into(),
            RowStatus::OneSided => "nothing on this side".into(),
        }
    }

    /// Which rows are in the scene `scene` names (a number, 1 = the first,
    /// or part of its heading, as `read_scene` takes it) on this side. A row
    /// is in the scene of its first paragraph on this side; a row with none
    /// there stays in the scene of the row above.
    pub fn in_scene(&self, scene: &Value) -> Result<Vec<bool>, ToolError> {
        let inside = crate::read::in_scene(&self.scripts[self.this], scene)?;
        let mut cur = false;
        Ok(self
            .rows
            .iter()
            .map(|r| {
                if let Some(&b) = r.cells[self.this].first().and_then(|id| inside.get(id)) {
                    cur = b;
                }
                cur
            })
            .collect())
    }

    /// The rows as shown, those of `scene` only (see [`Self::in_scene`]) and,
    /// with `needs_only`, those that need work only. `session` and `doc` are
    /// the session the suggestions were placed in (the version's), for their
    /// names, numbers and statuses.
    pub fn views(
        &self,
        session: &Session,
        doc: &Document,
        scene: Option<&Value>,
        needs_only: bool,
    ) -> Result<Vec<RowView>, ToolError> {
        let in_scene = scene.map(|q| self.in_scene(q)).transpose()?;
        let (this_m, other_m) = (self.this, self.other());
        let numbers: Vec<Vec<usize>> = session.reviews.iter().map(|r| item_numbers(doc, &r.items)).collect();
        let by_id: Vec<HashMap<ParaId, &Paragraph>> =
            self.scripts.iter().map(|s| s.paragraphs.iter().map(|p| (p.id, p)).collect()).collect();
        let side = |cell: &[ParaId], m: usize| -> Vec<ParaView> {
            cell.iter()
                .filter_map(|id| by_id[m].get(id))
                .map(|p| ParaView { id: p.id.0, kind: p.kind.name().to_string(), text: p.text() })
                .collect()
        };
        let mut out = vec![];
        for (k, r) in self.rows.iter().enumerate() {
            let needs = r.needs && self.on_version();
            if needs_only && !needs || in_scene.as_ref().is_some_and(|set| !set[k]) {
                continue;
            }
            let suggestions = r
                .suggestions
                .iter()
                .filter_map(|&n| self.placed.get(n))
                .filter(|(rv, i)| *rv < session.reviews.len() && *i < session.reviews[*rv].items.len())
                .map(|&(rv, i)| SuggestionView {
                    review: session.reviews[rv].name.clone(),
                    n: numbers[rv][i],
                    status: session.status_label(doc, rv, i),
                })
                .collect();
            out.push(RowView {
                row: k + 1,
                status: self.status_key(r.status),
                label: self.status_label(r.status),
                needs,
                note: r.note.map(|n| n.text()),
                this: side(&r.cells[this_m], this_m),
                other: side(&r.cells[other_m], other_m),
                suggestions,
            });
        }
        Ok(out)
    }
}
