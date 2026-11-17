//! The open script's pair of versions as the app shows it (plan step B1):
//! the pair as loaded, what to watch to load it again, its rows laid out
//! over the live document with an index from this script's paragraphs to
//! their rows, the original's lines shown dimmed in blank version lines,
//! and the session's choice under "Lined up on the left".
//!
//! No GPUI here: `ScriptView` owns one [`LinksView`], asks it whether the
//! rows are stale after each change ([`LinksView::stale`]) and lays them out
//! again at once after a structural edit, or once typing settles after a
//! text edit, as it does the outline. The rows come from
//! `LoadedPair::lay_out`, the call every host makes, so the app's rows are
//! the CLI's and the tools'.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use scriptorium_core::{Document, ParaId, Paragraph};
use scriptorium_fdx::FdxFile;
use scriptorium_fsio::versions::{LoadedPair, PairState, PlacedItem};
use scriptorium_review::Session;
use scriptorium_review::file::RowRef;
use scriptorium_review::sessionfile;
use scriptorium_versions::LaidRow;
use scriptorium_versions::links::Links;
use scriptorium_versions::rows::RowStatus;

/// The session's `links` value for "Nothing" under "Lined up on the left".
pub(crate) const NOTHING_ON_THE_LEFT: &str = "none";

/// The pair's rows as last laid out, and where this script's paragraphs are
/// in them. Read by B2's two columns (bands per paragraph) and B3's sidebar.
#[derive(Debug, Default)]
pub(crate) struct Rows {
    /// `LoadedPair::lay_out`'s rows: `cells` per member, in the links file's
    /// member order.
    pub rows: Vec<LaidRow>,
    /// The version's `ROW:` suggestions ([`LaidRow::suggestions`] indexes
    /// these); none on the original.
    #[allow(dead_code)] // Read by no view yet.
    pub placed: Vec<PlacedItem>,
    /// This script's paragraphs (its cells) → their row.
    row_of: HashMap<ParaId, usize>,
    /// This script's member index in `rows[_].cells`.
    this: usize,
}

impl Rows {
    fn new(rows: Vec<LaidRow>, placed: Vec<PlacedItem>, this: usize) -> Self {
        let row_of =
            rows.iter().enumerate().flat_map(|(r, row)| row.cells[this].iter().map(move |&id| (id, r))).collect();
        Self { rows, placed, row_of, this }
    }

    /// One [`Band`] per paragraph of `live` (the open script's paragraphs in
    /// order): what the editor draws beside it in two columns.
    ///
    /// Every live paragraph gets a band, and every row is drawn once:
    /// - the paragraphs of one row in a run (`slice` 0, 1, …) share its left
    ///   cell, which the first carries ([`Band::cell`]) and which flows down
    ///   over the run, the last padded to its end ([`Band::last`]);
    /// - a live paragraph no row lists (on the original, a line made in its
    ///   session; rows there come from its file) joins the row around it when
    ///   it sits inside that row's run, else it is a one-sided band of its own
    ///   (`row: None`) after the row above;
    /// - rows with nothing on this side (new lines of the original, C4, or
    ///   lines removed here) are drawn above the next paragraph
    ///   ([`Band::before`]), or below the last one ([`Band::after`]).
    ///
    /// Listed ids that aren't live (deleted in the original's session) are
    /// left out.
    pub fn bands(&self, live: &[ParaId]) -> Vec<Band> {
        bands(&self.rows, self.this, live)
    }

    /// The row this script's paragraph `para` is on.
    pub fn row_of(&self, para: ParaId) -> Option<usize> {
        self.row_of.get(&para).copied()
    }
}

/// A row's cell on the left: the row, its status, and the other member's
/// paragraphs (ids in its file) drawn there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeftCell {
    /// `None`: a line of this script no row lists (nothing on the left).
    pub row: Option<usize>,
    pub status: Option<RowStatus>,
    pub left: Vec<ParaId>,
}

/// What the editor draws beside one paragraph of the open script when the
/// other member is lined up on the left ([`Rows::bands`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Band {
    /// Rows with nothing on this side, drawn above the paragraph.
    pub before: Vec<LeftCell>,
    /// The paragraph's row. Only the first paragraph of a row's run carries
    /// the left paragraphs and the status; the rest of the run has `left`
    /// empty and `status` `None`.
    pub cell: LeftCell,
    /// Place in the row's run (0 for the first).
    pub slice: usize,
    /// The run's paragraphs before this one: the left cell flows down past
    /// them, so this one draws it from their height on.
    pub above: Vec<ParaId>,
    /// The last of the run: made as tall as what is left of the left cell.
    pub last: bool,
    /// On the script's last paragraph: rows with nothing on this side after
    /// it.
    pub after: Vec<LeftCell>,
}

/// [`Rows::bands`] over `rows`, for the member `this` (two members: the
/// other is `1 - this`).
pub(crate) fn bands(rows: &[LaidRow], this: usize, live: &[ParaId]) -> Vec<Band> {
    let other = usize::from(this == 0);
    let is_live: std::collections::HashSet<ParaId> = live.iter().copied().collect();
    let row_of: HashMap<ParaId, usize> = rows
        .iter()
        .enumerate()
        .flat_map(|(r, row)| row.cells[this].iter().filter(|id| is_live.contains(id)).map(move |&id| (id, r)))
        .collect();
    // Rows drawn above (or below) a paragraph: nothing live on this side,
    // something on the left.
    let left_only: Vec<bool> = rows
        .iter()
        .map(|row| !row.cells[other].is_empty() && !row.cells[this].iter().any(|id| is_live.contains(id)))
        .collect();

    // Each paragraph's row; an unlisted one joins the row around it when the
    // listed paragraphs on both sides of it are that row's.
    let listed: Vec<Option<usize>> = live.iter().map(|id| row_of.get(id).copied()).collect();
    let mut next_listed = vec![None; live.len()];
    let mut next = None;
    for i in (0..live.len()).rev() {
        next = listed[i].or(next);
        next_listed[i] = next;
    }
    let mut row_at = Vec::with_capacity(live.len());
    let mut prev = None;
    for i in 0..live.len() {
        let r = match listed[i] {
            Some(r) => Some(r),
            None if prev.is_some() && prev == next_listed[i] => prev,
            None => None,
        };
        if listed[i].is_some() {
            prev = listed[i];
        }
        row_at.push(r);
    }

    let cell = |r: usize| LeftCell { row: Some(r), status: Some(rows[r].status), left: rows[r].cells[other].clone() };
    let mut out = Vec::with_capacity(live.len());
    // Rows from here on not yet drawn.
    let mut next_row = 0;
    let mut drawn = vec![false; rows.len()];
    let mut i = 0;
    while i < live.len() {
        let r = row_at[i];
        let end = (i..live.len()).find(|&k| row_at[k] != r || (r.is_none() && k > i)).unwrap_or(live.len());
        let mut before = vec![];
        let mut head = LeftCell { row: r, status: None, left: vec![] };
        if let Some(r) = r {
            before = (next_row..r).filter(|&q| left_only[q] && !drawn[q]).map(cell).collect();
            for q in next_row..r {
                drawn[q] |= left_only[q];
            }
            next_row = next_row.max(r + 1);
            if !drawn[r] {
                drawn[r] = true;
                head = cell(r);
            }
        }
        for k in i..end {
            let first = k == i;
            out.push(Band {
                before: if first { std::mem::take(&mut before) } else { vec![] },
                cell: if first { head.clone() } else { LeftCell { row: r, status: None, left: vec![] } },
                slice: k - i,
                above: live[i..k].to_vec(),
                last: k + 1 == end,
                after: vec![],
            });
        }
        i = end;
    }
    let after: Vec<LeftCell> = (next_row..rows.len()).filter(|&q| left_only[q] && !drawn[q]).map(cell).collect();
    if let Some(last) = out.last_mut() {
        last.after = after;
    }
    out
}

/// Drop the status of rows in sync whose two sides read the same (copied
/// character cues, proper names): a mark on every cue would drown the ones
/// that say something. `this_text(k)`: the text of the paragraph of band
/// `k`; `left_text`: the other member's paragraph's text.
pub(crate) fn quiet_copies(
    bands: &mut [Band],
    this_text: impl Fn(usize) -> String,
    left_text: impl Fn(ParaId) -> Option<String>,
) {
    let mut k = 0;
    while k < bands.len() {
        let end = (k..bands.len()).find(|&j| bands[j].last).map_or(bands.len(), |j| j + 1);
        let b = &bands[k];
        if b.cell.status == Some(RowStatus::InSync) && b.cell.left.len() == end - k {
            let same = b.cell.left.iter().enumerate().all(|(i, id)| left_text(*id) == Some(this_text(k + i)));
            if same {
                bands[k].cell.status = None;
            }
        }
        k = end;
    }
}

/// What the editor needs to draw the other member on the left: a band per
/// paragraph of the open script, and the other member's paragraphs (from
/// its file) by id.
#[derive(Debug, Default)]
pub(crate) struct Columns {
    pub bands: HashMap<ParaId, Band>,
    pub left: Rc<HashMap<ParaId, Paragraph>>,
}

/// What has changed since the rows were laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stale {
    /// Nothing: the rows read the document and session as they are.
    No,
    /// Text only (typing, a kind, a decision's text): rows and the index
    /// are the same, statuses may not be. Lay out again once typing settles.
    Text,
    /// Paragraphs came or went, the reviews changed, or the pair was loaded
    /// again: the index is wrong. Lay out again now.
    Now,
}

/// What the rows were laid out for: the document (its history and log
/// length), its paragraph ids in order, and the reviews.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    origin: u64,
    log: usize,
    ids: Vec<ParaId>,
    reviews: Vec<(String, usize)>,
}

impl Key {
    fn of(session: &Session, doc: &Document) -> Self {
        Self {
            origin: doc.origin(),
            log: doc.log().len(),
            ids: doc.paragraphs().iter().map(|p| p.id).collect(),
            reviews: session.reviews.iter().map(|r| (r.id.clone(), r.items.len())).collect(),
        }
    }
}

/// The script's pair as the app holds it (module docs).
#[derive(Debug)]
pub(crate) struct LinksView {
    pair: Rc<PairState>,
    watch: PairWatch,
    ghosts: Rc<HashMap<ParaId, String>>,
    /// The other member's paragraphs by id (position in `other_script`).
    other_at: HashMap<ParaId, usize>,
    /// Load the pair again on the next look at the files.
    recheck: bool,
    /// The session's `links` field ([`sessionfile::SessionFile::links`]),
    /// checked ([`checked_left`]).
    left: Option<String>,
    /// `left` was chosen here and isn't in the session file yet: a reload
    /// keeps it rather than the file's ([`Self::left_from_session`]).
    left_unsaved: bool,
    rows: Rc<Rows>,
    /// What `rows` were laid out for; `None`: never, or the pair changed.
    key: Option<Key>,
    /// The other member's paragraphs by id, for the left column.
    left_paras: Rc<HashMap<ParaId, Paragraph>>,
    /// [`Self::columns`] as last made: for these rows and these live ids.
    columns: Option<(Rc<Rows>, Vec<ParaId>, Rc<Columns>)>,
}

impl LinksView {
    /// `pair`, just loaded for the script `script_name` in `dir`, with the
    /// session's choice `left`. The rows are laid out on the first
    /// [`Self::lay_out`].
    pub fn new(pair: PairState, dir: &Path, script_name: &str, left: Option<String>) -> Self {
        let mut v = Self {
            pair: Rc::default(),
            watch: PairWatch::default(),
            ghosts: Rc::default(),
            other_at: HashMap::new(),
            recheck: false,
            left: checked_left(left),
            left_unsaved: false,
            rows: Rc::default(),
            key: None,
            left_paras: Rc::default(),
            columns: None,
        };
        v.set_pair(pair, dir, script_name);
        v
    }

    /// The pair loaded again (a reload): watched afresh, rows stale.
    pub fn set_pair(&mut self, pair: PairState, dir: &Path, script_name: &str) {
        self.watch = PairWatch::of(&pair, dir, script_name);
        self.other_at = pair
            .loaded()
            .map(|p| p.other_script.paragraphs.iter().enumerate().map(|(i, q)| (q.id, i)).collect())
            .unwrap_or_default();
        self.ghosts = Rc::new(pair_ghosts(&pair, &self.other_at));
        self.left_paras = Rc::new(
            pair.loaded()
                .map(|p| p.other_script.paragraphs.iter().map(|q| (q.id, q.clone())).collect())
                .unwrap_or_default(),
        );
        self.columns = None;
        self.pair = Rc::new(pair);
        self.rows = Rc::default();
        self.key = None;
    }

    pub fn pair(&self) -> &PairState {
        &self.pair
    }

    /// The loaded pair, when it loaded.
    pub fn loaded(&self) -> Option<&LoadedPair> {
        self.pair.loaded()
    }

    /// [`pair_ghosts`] of the pair as loaded.
    pub fn ghosts(&self) -> Rc<HashMap<ParaId, String>> {
        self.ghosts.clone()
    }

    /// The other member's paragraph `id` (from its file): the left cell's
    /// text.
    #[allow(dead_code)] // B2's left column reads `left_paras`.
    pub fn other_paragraph(&self, id: ParaId) -> Option<&Paragraph> {
        let p = self.pair.loaded()?;
        self.other_at.get(&id).map(|&i| &p.other_script.paragraphs[i])
    }

    /// Load the pair again on the next look at the files (a version was
    /// just made).
    pub fn recheck_soon(&mut self) {
        self.recheck = true;
    }

    /// Whether to load the pair again now: asked for, or its files changed
    /// ([`PairWatch`]). Asking clears a request.
    pub fn due(&mut self) -> bool {
        std::mem::take(&mut self.recheck) || self.watch.changed()
    }

    /// Whether the pair's files changed since it was loaded (a request to
    /// load it again is left as it is).
    pub fn files_changed(&mut self) -> bool {
        self.watch.changed()
    }

    /// The session's choice under "Lined up on the left", as saved
    /// ([`sessionfile::SessionFile::links`]).
    pub fn left_choice(&self) -> Option<&str> {
        self.left.as_deref()
    }

    /// The writer changed the choice (the sidebar, Swap). The caller saves
    /// the session at once: until [`Self::left_saved`], a reload keeps this
    /// choice rather than the session file's. View-only state: no CLI
    /// command sets it (plan B3).
    pub fn set_left_choice(&mut self, choice: Option<String>) {
        self.left = checked_left(choice);
        self.left_unsaved = true;
    }

    /// The session file's choice, read again by a reload: taken unless one
    /// chosen here isn't saved yet.
    pub fn left_from_session(&mut self, choice: Option<String>) {
        if !self.left_unsaved {
            self.left = checked_left(choice);
        }
    }

    /// The session file now holds [`Self::left_choice`].
    pub fn left_saved(&mut self) {
        self.left_unsaved = false;
    }

    /// Whether the other member is shown lined up on the left: the pair
    /// loaded and the choice isn't "Nothing". (One pair per script for
    /// now, so a named links file is the pair's: a name that isn't the
    /// loaded pair's file, say one renamed or removed since, is advisory and
    /// falls back to the pair.)
    pub fn shows_left(&self) -> bool {
        self.pair.loaded().is_some() && self.left.as_deref() != Some(NOTHING_ON_THE_LEFT)
    }

    /// The two columns for the live `doc` while the other member is shown
    /// on the left ([`Self::shows_left`]); made again only when the rows
    /// were laid out again or paragraphs came or went (on the original the
    /// rows stay, but its new lines need bands).
    pub fn columns(&mut self, doc: &Document) -> Option<Rc<Columns>> {
        if !self.shows_left() {
            return None;
        }
        let ids: Vec<ParaId> = doc.paragraphs().iter().map(|p| p.id).collect();
        if let Some((rows, have, c)) = &self.columns
            && Rc::ptr_eq(rows, &self.rows)
            && *have == ids
        {
            return Some(c.clone());
        }
        let mut bands = self.rows.bands(&ids);
        let paras = doc.paragraphs();
        let left = &self.left_paras;
        quiet_copies(&mut bands, |k| paras[k].text(), |id| left.get(&id).map(|p| p.text()));
        let c = Rc::new(Columns { bands: ids.iter().copied().zip(bands).collect(), left: self.left_paras.clone() });
        self.columns = Some((self.rows.clone(), ids, c.clone()));
        Some(c)
    }

    /// The rows as last laid out (empty without a loaded pair).
    pub fn rows(&self) -> Rc<Rows> {
        self.rows.clone()
    }

    /// What changed since the rows were laid out, for `session` and its live
    /// `doc`. On the original the rows read both files, not the document,
    /// so only a new pair makes them stale.
    pub fn stale(&self, session: &Session, doc: &Document) -> Stale {
        let Some(p) = self.pair.loaded() else { return Stale::No };
        let Some(key) = &self.key else { return Stale::Now };
        if !p.this_is_version() {
            return Stale::No;
        }
        if key.origin != doc.origin()
            || key.reviews.len() != session.reviews.len()
            || key.reviews.iter().zip(&session.reviews).any(|((id, n), r)| *id != r.id || *n != r.items.len())
            || key.ids.len() != doc.paragraphs().len()
            || key.ids.iter().zip(doc.paragraphs()).any(|(id, p)| *id != p.id)
        {
            return Stale::Now;
        }
        if key.log != doc.log().len() { Stale::Text } else { Stale::No }
    }

    /// Lay the rows out again (`LoadedPair::lay_out`) for `this` (the
    /// script's file as opened), `session` and its live `doc`.
    pub fn lay_out(&mut self, this: &FdxFile, session: &Session, doc: &Document) {
        let Some(p) = self.pair.loaded() else {
            self.rows = Rc::default();
            self.key = None;
            return;
        };
        let laid = p.lay_out(this, session, doc);
        self.rows = Rc::new(Rows::new(laid.rows, laid.placed, p.this));
        self.key = Some(Key::of(session, doc));
    }

    /// What the card says about a `ROW:` suggestion's row (`row`), on the
    /// version of a loaded pair.
    pub fn row_card(&self, row: &RowRef) -> Option<RowCard> {
        let p = self.pair.loaded().filter(|p| p.this_is_version())?;
        Some(row_card(&p.links, row))
    }

    /// The status bar's note on opening the original of a pair.
    pub fn note(&self) -> Option<String> {
        pair_note(&self.pair)
    }
}

/// The session's `links` value as the app uses it: "Nothing"
/// ([`NOTHING_ON_THE_LEFT`]) or a plain `*.links.txt` name (no `/`, `\`,
/// `:`, `..` or leading `.`); anything else is read as no choice (the pair).
/// The session file is written by other programs too, so the name is never
/// trusted as a path.
pub(crate) fn checked_left(choice: Option<String>) -> Option<String> {
    let c = choice?;
    let plain = !c.contains(['/', '\\', ':'])
        && !c.contains("..")
        && !c.starts_with('.')
        && !c.chars().any(char::is_control)
        && c.to_lowercase().ends_with(".links.txt");
    (c == NOTHING_ON_THE_LEFT || plain).then_some(c)
}

/// The status bar's note on opening the original of a pair: its rows are
/// translated (or, two files in one language, reviewed) in the version.
pub(crate) fn pair_note(pair: &PairState) -> Option<String> {
    let p = pair.loaded()?;
    if p.this_is_version() {
        return None;
    }
    let version = &p.links.members[p.links.sides().1].file;
    let what = if p.links.same_language() { "review it" } else { "translate" };
    Some(format!("Lined up with {version}: open it (Swap, in the toolbar) to {what}."))
}

/// A file's modification time and length: looked at every tick, before
/// reading it again.
type FileSig = Option<(std::time::SystemTime, u64)>;

fn file_sig(path: &Path) -> FileSig {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The `*.links.txt` files in `dir`, by name.
fn links_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.to_lowercase().ends_with(".links.txt"))
        .collect();
    names.sort();
    names
}

/// How long after a pair couldn't be used to try loading it again: a links
/// lock held a moment, or Final Draft halfway through saving the other
/// file. Then only a change to its files loads it again.
const PAIR_RETRIES: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// What the app looks at every tick to know when to load the script's pair
/// again: loading may re-find and write the links file, so it isn't done on
/// every tick, only when
/// - a file of the pair changed: the links file or the other member (time
///   and length first, then, only then, its bytes against the hashes the
///   pair was loaded from);
/// - without a pair, a `*.links.txt` file came or went in the folder (a
///   version made by the CLI or Claude while the original is open);
/// - a pair that couldn't be used is due another try ([`PAIR_RETRIES`]).
#[derive(Debug, Default)]
pub(crate) struct PairWatch {
    pub(crate) files: Vec<Watched>,
    /// Without a pair: the folder and its links files when loaded.
    folder: Option<(PathBuf, Vec<String>)>,
    /// An unusable pair: when to try again, and the tries left after that.
    pub(crate) retry: Option<(Instant, usize)>,
}

#[derive(Debug)]
pub(crate) struct Watched {
    pub(crate) path: PathBuf,
    /// `sessionfile::hash` of the bytes the pair was loaded from; `None`:
    /// any change counts (a pair that couldn't be used).
    hash: Option<String>,
    /// The file's [`FileSig`] when its bytes were last found to be `hash`;
    /// `None` until then.
    seen: Option<FileSig>,
}

impl PairWatch {
    /// What to look at for `pair`, just loaded for the script `script_name`
    /// in `dir`.
    pub(crate) fn of(pair: &PairState, dir: &Path, script_name: &str) -> Self {
        let any_change = |path: PathBuf| Watched { seen: Some(file_sig(&path)), path, hash: None };
        match pair {
            PairState::None => Self { folder: Some((dir.to_path_buf(), links_files(dir))), ..Self::default() },
            PairState::Loaded(p) => Self {
                files: vec![
                    Watched { path: p.path.clone(), hash: Some(p.links_hash.clone()), seen: None },
                    Watched { path: p.other_path.clone(), hash: Some(p.other_hash.clone()), seen: None },
                ],
                ..Self::default()
            },
            PairState::Unusable { links, .. } => {
                let mut files = vec![];
                if let Some(l) = links {
                    // The other member too: Final Draft finishing its save
                    // makes the pair usable.
                    let other = std::fs::read_to_string(l)
                        .ok()
                        .and_then(|t| scriptorium_versions::links::parse(&t).ok())
                        .and_then(|x| {
                            x.member_of(script_name).map(|m| x.members[if m == 0 { 1 } else { 0 }].file.clone())
                        });
                    files.push(any_change(l.clone()));
                    files.extend(other.map(|o| any_change(dir.join(o))));
                }
                let retry = Some((Instant::now() + PAIR_RETRIES[0], 1));
                Self { files, folder: None, retry }
            }
        }
    }

    /// Whether to load the pair again now (see [`PairWatch`]).
    pub(crate) fn changed(&mut self) -> bool {
        if let Some((at, n)) = self.retry
            && Instant::now() >= at
        {
            self.retry = PAIR_RETRIES.get(n).map(|d| (Instant::now() + *d, n + 1));
            return true;
        }
        if let Some((dir, names)) = &self.folder {
            return links_files(dir) != *names;
        }
        for w in &mut self.files {
            // Before reading: a write after this shows as a newer time later.
            let sig = file_sig(&w.path);
            if w.seen == Some(sig) {
                continue;
            }
            let Some(hash) = &w.hash else { return true };
            match std::fs::read_to_string(&w.path) {
                Ok(text) if sessionfile::hash(&text) == *hash => w.seen = Some(sig),
                _ => return true,
            }
        }
        false
    }
}

/// On the version of a loaded pair: for each of its lines, the original's
/// line(s) of its row, shown dimmed while the line is blank (what is left to
/// translate reads in place). Rows of one version line and several original
/// lines show them all; several version lines pair with the original's in
/// order where the counts agree, else the first takes them all. `other_at`:
/// the original's paragraphs by id (positions in `other_script`).
pub(crate) fn pair_ghosts(pair: &PairState, other_at: &HashMap<ParaId, usize>) -> HashMap<ParaId, String> {
    let Some(p) = pair.loaded().filter(|p| p.this_is_version()) else { return HashMap::new() };
    let (o, v) = p.links.sides();
    let text = |id: &ParaId| other_at.get(id).map(|&i| p.other_script.paragraphs[i].text());
    let mut out = HashMap::new();
    for row in &p.links.rows {
        let (orig, ver) = (&row.cells[o], &row.cells[v]);
        let texts: Vec<String> = orig.iter().filter_map(text).collect();
        if ver.len() == texts.len() {
            out.extend(ver.iter().zip(&texts).map(|(id, t)| (*id, t.clone())));
        } else if let Some(first) = ver.first() {
            out.insert(*first, texts.join(" "));
        }
    }
    out.retain(|_, t| !t.trim().is_empty());
    out
}

/// What the card shows above a `ROW:` suggestion: the original's line(s) it
/// was made for (`ROW TEXT`, which the original's file still reads: an
/// entry whose original line changed isn't placed, so it has no card).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowCard {
    /// The original's label ("English", or its file stem).
    pub label: String,
    /// The version's label ("Español").
    pub version: String,
    /// `ROW TEXT`, one line per paragraph.
    pub text: String,
    /// What the card says for the version's line while it is blank.
    pub blank: &'static str,
}

/// The card's [`RowCard`] for `row` (an item's `ROW:`) in the pair `links`.
pub(crate) fn row_card(links: &Links, row: &RowRef) -> RowCard {
    let (o, v) = links.sides();
    let blank = if links.same_language() { "empty" } else { "empty \u{2014} not yet translated" };
    RowCard { label: links.label(o), version: links.label(v), text: row.texts.join("\n"), blank }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_core::{Pos, Selection};
    use scriptorium_fsio::versions;
    use scriptorium_review::sessionfile::ResumedSession;

    const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading">
      <Text>EXT. FARM HOUSE - NIGHT</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Laura smiles.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>I am going to town tomorrow.</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Laura laughs.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

    /// A folder holding `raw` as `film.fdx` and its version `film.es.fdx`
    /// (`version new`); removed on drop.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str, raw: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("scriptorium-app-links-view-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap();
            std::fs::write(dir.join("film.fdx"), raw).unwrap();
            versions::version_new(&dir.join("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
            Self(dir)
        }

        /// The script `name` opened as the app opens it: its file, its
        /// session (fresh) and its pair.
        fn open(&self, name: &str) -> (FdxFile, ResumedSession, LinksView) {
            let fdx = FdxFile::parse(std::fs::read_to_string(self.0.join(name)).unwrap()).unwrap();
            let (opened, pair) = versions::resume(&fdx, &self.0, name, vec![], None, &mut vec![]);
            assert!(pair.loaded().is_some(), "{pair:?}");
            let view = LinksView::new(pair, &self.0, name, opened.links.clone());
            (fdx, opened, view)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The rows' cells and this script's index from a fresh `lay_out`.
    fn fresh(view: &LinksView, fdx: &FdxFile, o: &ResumedSession) -> (Vec<Vec<Vec<ParaId>>>, HashMap<ParaId, usize>) {
        let p = view.loaded().unwrap();
        let laid = p.lay_out(fdx, &o.session, &o.doc);
        let rows = Rows::new(laid.rows, laid.placed, p.this);
        (rows.rows.iter().map(|r| r.cells.clone()).collect(), rows.row_of)
    }

    /// The view's rows and index read as a fresh `lay_out` does, for every
    /// paragraph of the document.
    fn assert_matches_fresh(view: &LinksView, fdx: &FdxFile, o: &ResumedSession, what: &str) {
        let (cells, row_of) = fresh(view, fdx, o);
        let rows = view.rows();
        let have: Vec<Vec<Vec<ParaId>>> = rows.rows.iter().map(|r| r.cells.clone()).collect();
        assert_eq!(have, cells, "cells after {what}");
        for p in o.doc.paragraphs() {
            assert_eq!(rows.row_of(p.id), row_of.get(&p.id).copied(), "row of {:?} after {what}", p.id);
        }
        assert_eq!(rows.row_of, row_of, "index after {what}");
    }

    /// Lay out when stale as the app does: at once on [`Stale::Now`]; on
    /// [`Stale::Text`] the rows kept until then must already be right.
    fn settle(view: &mut LinksView, fdx: &FdxFile, o: &ResumedSession, what: &str) {
        match view.stale(&o.session, &o.doc) {
            Stale::No => {}
            Stale::Now => view.lay_out(fdx, &o.session, &o.doc),
            Stale::Text => {
                assert_matches_fresh(view, fdx, o, &format!("{what} (before settling)"));
                view.lay_out(fdx, &o.session, &o.doc);
            }
        }
        assert_eq!(view.stale(&o.session, &o.doc), Stale::No, "{what}");
        assert_matches_fresh(view, fdx, o, what);
    }

    fn at(doc: &Document, i: usize, char_offset: usize) -> Selection {
        let p = &doc.paragraphs()[i];
        let text = p.text();
        let offset = text.char_indices().nth(char_offset).map_or(text.len(), |(b, _)| b);
        Selection::cursor(Pos::new(p.id, offset))
    }

    /// Typing, splitting and joining paragraphs at `i` (a line with text),
    /// checking the rows after each step.
    fn edit_at(view: &mut LinksView, fdx: &FdxFile, o: &mut ResumedSession, i: usize) {
        let sel = at(&o.doc, i, 0);
        o.doc.insert_text(sel, "Hola ").unwrap();
        assert_eq!(view.stale(&o.session, &o.doc), Stale::Text, "typing at {i}");
        settle(view, fdx, o, &format!("typing at {i}"));
        let sel = at(&o.doc, i, 3);
        o.doc.enter(sel).unwrap();
        assert_eq!(view.stale(&o.session, &o.doc), Stale::Now, "split at {i}");
        settle(view, fdx, o, &format!("split at {i}"));
        // A new blank line after it: an unlisted line placed by D3.
        let len = o.doc.paragraphs()[i + 1].text().chars().count();
        let sel = at(&o.doc, i + 1, len);
        o.doc.enter(sel).unwrap();
        settle(view, fdx, o, &format!("a new line after {i}"));
        // Joined back, then the original line joined onto the one above.
        let sel = at(&o.doc, i + 2, 0);
        o.doc.delete_backward(sel, false).unwrap();
        settle(view, fdx, o, &format!("joining the new line at {i}"));
        let sel = at(&o.doc, i + 1, 0);
        o.doc.delete_backward(sel, false).unwrap();
        settle(view, fdx, o, &format!("joining the split at {i}"));
        if i > 0 {
            let sel = at(&o.doc, i, 0);
            o.doc.delete_backward(sel, false).unwrap();
            settle(view, fdx, o, &format!("joining {i} onto the line above"));
        }
        o.doc.undo().unwrap();
        settle(view, fdx, o, &format!("undo at {i}"));
    }

    #[test]
    fn the_row_index_after_edits_matches_a_fresh_lay_out() {
        let f = Folder::new("index", SCRIPT);
        let (fdx, mut o, mut view) = f.open("film.es.fdx");
        assert_eq!(view.stale(&o.session, &o.doc), Stale::Now, "never laid out");
        settle(&mut view, &fdx, &o, "opening");
        assert_eq!(view.rows().rows.len(), 5);
        for i in [1, 3, 4] {
            edit_at(&mut view, &fdx, &mut o, i);
        }
        // A new pair (a reload): stale until laid out again.
        let (_, _, again) = f.open("film.es.fdx");
        view.set_pair(again.pair().clone(), &f.0, "film.es.fdx");
        assert_eq!(view.stale(&o.session, &o.doc), Stale::Now);
        settle(&mut view, &fdx, &o, "a reload");
    }

    /// On the original the rows read both files, not the live document:
    /// laid out once per pair, never stale from typing.
    #[test]
    fn on_the_original_the_rows_are_laid_out_once() {
        let f = Folder::new("original", SCRIPT);
        let (fdx, mut o, mut view) = f.open("film.fdx");
        assert!(view.ghosts().is_empty());
        assert_eq!(view.stale(&o.session, &o.doc), Stale::Now);
        view.lay_out(&fdx, &o.session, &o.doc);
        let sel = at(&o.doc, 1, 0);
        o.doc.enter(sel).unwrap();
        assert_eq!(view.stale(&o.session, &o.doc), Stale::No);
        let original = view.loaded().unwrap().this;
        let rows = view.rows();
        assert_eq!(rows.row_of(o.doc.paragraphs()[0].id), Some(0));
        assert_eq!(rows.rows[1].cells[original], vec![ParaId(1)]);
        // The version's paragraph by id, for the left column.
        let other = 1 - original;
        let cue = rows.rows[2].cells[other][0];
        assert_eq!(view.other_paragraph(cue).unwrap().text(), "CESAR");
    }

    // ---- bands (B2) ----------------------------------------------------------

    fn ids(v: &[u64]) -> Vec<ParaId> {
        v.iter().map(|&i| ParaId(i)).collect()
    }

    /// A row `this | other` for member 0 = this, 1 = other.
    fn row(this: &[u64], other: &[u64]) -> LaidRow {
        let status = if this.is_empty() || other.is_empty() { RowStatus::OneSided } else { RowStatus::InSync };
        LaidRow { cells: vec![ids(this), ids(other)], status, needs: false, note: None, suggestions: vec![] }
    }

    /// The invariants every band list keeps: one band per live paragraph,
    /// each row drawn once (its left paragraphs once, in order), runs
    /// numbered from 0 with the last marked, `above` the run so far.
    fn check(rows: &[LaidRow], this: usize, live: &[ParaId]) -> Vec<Band> {
        let bands = bands(rows, this, live);
        assert_eq!(bands.len(), live.len());
        let mut drawn: Vec<usize> = vec![];
        let mut left: Vec<ParaId> = vec![];
        for (k, b) in bands.iter().enumerate() {
            for c in b.before.iter().chain([&b.cell]).chain(&b.after) {
                if !c.left.is_empty() || c.status.is_some() {
                    drawn.push(c.row.unwrap());
                    left.extend(&c.left);
                }
            }
            assert_eq!(b.above, live[k - b.slice..k].to_vec(), "{k}");
            if b.slice > 0 {
                assert_eq!(bands[k - 1].cell.row, b.cell.row, "{k}");
                assert!(!bands[k - 1].last, "{k}");
            }
            if !b.last {
                assert_eq!(bands[k + 1].slice, b.slice + 1, "{k}");
            }
            if k + 1 < bands.len() {
                assert!(b.after.is_empty());
            }
        }
        let mut sorted = drawn.clone();
        sorted.sort();
        assert_eq!(drawn, sorted, "rows drawn in order");
        sorted.dedup();
        assert_eq!(drawn, sorted, "each row drawn once");
        let other = 1 - this;
        let want: Vec<ParaId> = rows.iter().flat_map(|r| r.cells[other].clone()).collect();
        assert_eq!(left, want, "every left paragraph drawn once, in order");
        bands
    }

    #[test]
    fn bands_for_one_to_two_two_to_one_and_one_sided_rows() {
        let rows = vec![
            row(&[0], &[10]),
            row(&[1, 2], &[11]),  // 1:2 (two here, one on the left)
            row(&[3], &[12, 13]), // 2:1
            row(&[], &[14]),      // left only (a new line of the original)
            row(&[4], &[]),       // nothing on the left
            row(&[5], &[15]),
            row(&[], &[16, 17]), // left only, at the end
        ];
        let live = ids(&[0, 1, 2, 3, 4, 5]);
        let b = check(&rows, 0, &live);
        assert_eq!(b[1].cell.left, ids(&[11]));
        assert_eq!((b[1].slice, b[1].last, b[2].slice, b[2].last), (0, false, 1, true));
        assert!(b[2].cell.left.is_empty() && b[2].cell.status.is_none());
        assert_eq!(b[2].above, ids(&[1]));
        assert_eq!(b[3].cell.left, ids(&[12, 13]));
        assert!(b[3].last && b[3].before.is_empty());
        assert_eq!(b[4].before.len(), 1);
        assert_eq!(b[4].before[0].left, ids(&[14]));
        assert_eq!(b[4].cell.row, Some(4));
        assert!(b[4].cell.left.is_empty());
        assert_eq!(b[4].cell.status, Some(RowStatus::OneSided));
        assert_eq!(b[5].after.len(), 1);
        assert_eq!(b[5].after[0].left, ids(&[16, 17]));
    }

    /// On the original: lines made in its session (no row) and lines
    /// deleted in it (still in the rows, from its file).
    #[test]
    fn bands_for_unlisted_and_deleted_lines() {
        let rows = vec![row(&[0], &[10]), row(&[1, 2], &[11]), row(&[3], &[12]), row(&[4], &[13])];
        // 90 before everything; 91 inside row 1's run; 92 after row 2;
        // 3 deleted; 93, 94 at the end.
        let live = ids(&[90, 0, 1, 91, 2, 92, 4, 93, 94]);
        let b = check(&rows, 0, &live);
        assert_eq!(b[0].cell, LeftCell { row: None, status: None, left: vec![] });
        assert_eq!(b[2].cell.row, Some(1));
        assert_eq!((b[3].cell.row, b[3].slice, b[4].slice, b[4].last), (Some(1), 1, 2, true));
        assert_eq!(b[5].cell.row, None);
        // Row 2 lost its only line here: drawn above the next one.
        assert_eq!(b[6].before.len(), 1);
        assert_eq!(b[6].before[0].left, ids(&[12]));
        assert_eq!((b[7].cell.row, b[8].cell.row, b[7].slice, b[8].slice), (None, None, 0, 0));
        // Nothing live at all: every row hangs nowhere but stays drawn.
        assert!(bands(&rows, 0, &[]).is_empty());
        // No rows yet (not laid out): every line one-sided.
        assert!(check(&[], 0, &live).iter().all(|b| b.cell.row.is_none()));
    }

    /// The other side, as on the version (member 1 is this script).
    #[test]
    fn bands_for_member_one() {
        let rows = vec![row(&[10], &[0]), row(&[11], &[1, 2]), row(&[], &[3])];
        let live = ids(&[0, 1, 2, 3]);
        let b = check(&rows, 1, &live);
        assert_eq!(b[1].cell.left, ids(&[11]));
        assert!(b[3].cell.left.is_empty() && b[3].cell.row == Some(2));
    }

    /// Every paragraph of either member gets a band, through edits that
    /// split, add and join lines (on the original its new lines have no
    /// row), and every left paragraph is drawn once.
    #[test]
    fn every_live_paragraph_has_a_band_on_both_members() {
        let f = Folder::new("bands", SCRIPT);
        for name in ["film.es.fdx", "film.fdx"] {
            let (fdx, mut o, mut view) = f.open(name);
            let this = view.loaded().unwrap().this;
            let check_now = |view: &mut LinksView, o: &ResumedSession, what: &str| {
                if view.stale(&o.session, &o.doc) != Stale::No {
                    view.lay_out(&fdx, &o.session, &o.doc);
                }
                let live: Vec<ParaId> = o.doc.paragraphs().iter().map(|p| p.id).collect();
                let rows = view.rows();
                let laid: Vec<LaidRow> = rows.rows.clone();
                check(&laid, this, &live);
                let c = view.columns(&o.doc).unwrap();
                for id in &live {
                    assert!(c.bands.contains_key(id), "{name} {what}: {id:?}");
                }
                for b in c.bands.values() {
                    for l in b.before.iter().chain([&b.cell]).chain(&b.after).flat_map(|c| &c.left) {
                        assert!(c.left.contains_key(l), "{name} {what}: left {l:?}");
                    }
                }
            };
            check_now(&mut view, &o, "opening");
            let sel = at(&o.doc, 1, 3);
            o.doc.enter(sel).unwrap();
            check_now(&mut view, &o, "split");
            let len = o.doc.paragraphs()[4].text().len();
            o.doc.enter(Selection::cursor(Pos::new(o.doc.paragraphs()[4].id, len))).unwrap();
            check_now(&mut view, &o, "new line");
            let sel = at(&o.doc, 0, 0);
            o.doc.enter(sel).unwrap();
            check_now(&mut view, &o, "new first line");
            let sel = at(&o.doc, 4, 0);
            o.doc.delete_backward(sel, false).unwrap();
            check_now(&mut view, &o, "join");
            // Nothing on the left: no columns.
            view.set_left_choice(Some(NOTHING_ON_THE_LEFT.into()));
            assert!(view.columns(&o.doc).is_none());
        }
    }

    #[test]
    fn copied_rows_in_sync_show_no_status() {
        let rows = vec![row(&[0], &[10]), row(&[1], &[11]), row(&[2, 3], &[12, 13])];
        let mut b = bands(&rows, 0, &ids(&[0, 1, 2, 3]));
        let this = ["CESAR", "Hola", "GRANDMA", "x"];
        let left = |id: ParaId| Some(["CESAR", "Hello", "GRANDMA", "x"][id.0 as usize - 10].to_string());
        quiet_copies(&mut b, |k| this[k].to_string(), left);
        assert_eq!(b[0].cell.status, None, "copied cue");
        assert_eq!(b[1].cell.status, Some(RowStatus::InSync), "translated");
        assert_eq!(b[2].cell.status, None, "copied run of two");
        // Not in sync: kept, even when it reads the same.
        let mut rows = rows;
        rows[0].status = RowStatus::ThisChanged;
        let mut b = bands(&rows, 0, &ids(&[0, 1, 2, 3]));
        quiet_copies(&mut b, |k| this[k].to_string(), left);
        assert_eq!(b[0].cell.status, Some(RowStatus::ThisChanged));
    }

    /// A row whose lines here are split apart by another row's line (out
    /// of order): its left cell is drawn once, with the first part; the
    /// second part has nothing on the left (hatched) and no status.
    #[test]
    fn bands_for_a_row_split_apart_by_another() {
        let rows = vec![row(&[0], &[10]), row(&[1, 3], &[11]), row(&[2], &[12])];
        let b = check(&rows, 0, &ids(&[0, 1, 2, 3]));
        assert_eq!((b[1].cell.row, b[1].cell.left.clone(), b[1].last), (Some(1), ids(&[11]), true));
        assert_eq!(b[2].cell.left, ids(&[12]));
        assert_eq!(b[3].cell, LeftCell { row: Some(1), status: None, left: vec![] });
        assert!(b[3].above.is_empty() && b[3].slice == 0 && b[3].before.is_empty());
    }

    #[test]
    fn the_left_choice() {
        let f = Folder::new("choice", SCRIPT);
        let (_, _, mut view) = f.open("film.es.fdx");
        assert_eq!(view.left_choice(), None);
        assert!(view.shows_left());
        view.set_left_choice(Some(NOTHING_ON_THE_LEFT.into()));
        assert!(!view.shows_left());
        // Not saved yet: a reload keeps it; once saved, the file's wins.
        view.left_from_session(None);
        assert!(!view.shows_left());
        view.left_saved();
        view.left_from_session(Some("film.es.links.txt".into()));
        assert_eq!(view.left_choice(), Some("film.es.links.txt"));
        assert!(view.shows_left());
        // A stale name falls back to the pair; an unsafe one is dropped.
        view.left_from_session(Some("old.links.txt".into()));
        assert!(view.shows_left());
        for bad in ["../x.links.txt", "a/b.links.txt", ".x.links.txt", "x.txt", "C:x.links.txt", "a\\b.links.txt"] {
            assert_eq!(checked_left(Some(bad.into())), None, "{bad}");
        }
        assert_eq!(checked_left(Some("PURPLE LIST.es.links.txt".into())).as_deref(), Some("PURPLE LIST.es.links.txt"));
        let none = LinksView::new(PairState::None, &f.0, "film.es.fdx", None);
        assert!(!none.shows_left());
        assert_eq!(none.stale(&Session::default(), &Document::new(Default::default())), Stale::No);
    }

    /// The pair state on the real script (a version made of a copy): the
    /// index stays right through edits across the script, and what laying
    /// out and the staleness check cost (`--nocapture` prints it).
    #[test]
    fn the_real_script() {
        let real = std::env::var("SCRIPTORIUM_REAL_FDX")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../2026_11_05_PURPLE LIST.fdx"));
        let Ok(raw) = std::fs::read_to_string(&real) else {
            eprintln!("skipped: {} not found", real.display());
            return;
        };
        let f = Folder::new("real", &raw);
        let started = Instant::now();
        let (fdx, mut o, mut view) = f.open("film.es.fdx");
        let opened = started.elapsed();
        view.lay_out(&fdx, &o.session, &o.doc);
        let n = o.doc.paragraphs().len();
        assert!(n > 1000, "{n}");

        const RUNS: u32 = 20;
        let t = Instant::now();
        for _ in 0..RUNS {
            view.lay_out(&fdx, &o.session, &o.doc);
        }
        let lay_out = t.elapsed() / RUNS;
        let t = Instant::now();
        for _ in 0..RUNS {
            assert_eq!(view.stale(&o.session, &o.doc), Stale::No);
        }
        let stale = t.elapsed() / RUNS;
        eprintln!(
            "real script: {n} paragraphs, {} rows; open (version new's files, re-find, resume) {opened:?}; lay_out \
             {lay_out:?}; stale check {stale:?}",
            view.rows().rows.len()
        );
        // The two columns' bands: made again after each lay-out or when
        // paragraphs come or go; looked up (cached) on every other change.
        let t = Instant::now();
        for _ in 0..RUNS {
            view.columns = None;
            view.columns(&o.doc).unwrap();
        }
        let bands = t.elapsed() / RUNS;
        let t = Instant::now();
        for _ in 0..RUNS {
            view.columns(&o.doc).unwrap();
        }
        let cached = t.elapsed() / RUNS;
        eprintln!("real script: bands {bands:?}; columns when nothing changed {cached:?}");
        assert!(bands < Duration::from_millis(200), "{bands:?}");
        // Generous (debug builds, busy machines): a stale check is per change.
        assert!(stale < Duration::from_millis(50), "{stale:?}");
        assert!(lay_out < Duration::from_secs(2), "{lay_out:?}");

        // Edits across the script, on lines with text.
        let with_text: Vec<usize> = (1..n - 3).filter(|&i| o.doc.paragraphs()[i].text().chars().count() > 4).collect();
        for k in [0, with_text.len() / 3, with_text.len() / 2, with_text.len() - 1] {
            edit_at(&mut view, &fdx, &mut o, with_text[k]);
        }
    }
}
