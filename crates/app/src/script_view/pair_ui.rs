//! The script's pair in the window (plan step B3): the sidebar's "Lined up
//! on the left" section (and its button on the rail), what the toolbar
//! names and Swap opens, Edit › Mark in sync / Mark not in sync, making a
//! version from File › New version… (the sheet is `crate::new_version`).
//!
//! The choice under "Lined up on the left" is view-only state, kept in the
//! session's `links` field; no CLI command sets it. It is saved as soon as
//! it changes, since a reload reads it back from the session file. Mark in
//! sync writes only the links file (`scriptorium_fsio::versions::edit_row`,
//! as the CLI's `links sync` does): it is not an undo step, and the rows
//! catch up when the pair is loaded again (the links file changed), never
//! by editing the laid-out rows in place (after a Mark the pair is loaded
//! again at once, so a second Mark right away works).

use std::path::PathBuf;
use std::time::Duration;

use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use scriptorium_fsio::versions::{self, NewVersion, PairState, RowEdit, check_version_args, edit_row};
use scriptorium_versions::RowStatus;

use super::{RailTip, ScriptView, anchor_para, file_name, section_label, view_row};
use crate::disk::Plan;
use crate::elements::{STATUS_LEGEND, status_look};
use crate::links_view::NOTHING_ON_THE_LEFT;
use crate::theme::*;

/// What the toolbar says about the pair while the other file is lined up
/// on the left, in the columns' order: "English | **Español**" (the open
/// file's label is the bold one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairHeader {
    pub left: String,
    pub right: String,
    /// The left column gave way for width (shown dimmed).
    pub left_hidden: bool,
}

impl ScriptView {
    /// The writer chose what is lined up on the left (`None` or a links
    /// file name: the pair; [`NOTHING_ON_THE_LEFT`]): saved at once, so a
    /// reload doesn't read the old choice back. If the session can't be
    /// written now (another program is writing, or a rewrite is waiting on
    /// its changes), the autosave writes it, and a reload until then keeps
    /// this choice (`LinksView::left_from_session`).
    pub(crate) fn choose_left(&mut self, choice: Option<String>, cx: &mut Context<Self>) {
        if self.retired || self.links.left_choice() == choice.as_deref() {
            return;
        }
        self.links.set_left_choice(choice);
        // The editor's columns follow the choice.
        self.refresh(cx);
        cx.notify();
        if self.stuck.is_some() {
            return;
        }
        // The UI thread doesn't wait: busy, the autosave tries again.
        let Some(_lock) = self.lock_session(Duration::ZERO) else {
            self.schedule_autosave(cx);
            return;
        };
        match self.disk_plan().1 {
            Plan::Save => _ = self.write_session(cx),
            // What the other program wrote first (the choice made here is
            // kept), then the choice, under the same lock.
            Plan::Reload => {
                self.reload(cx);
                if self.stuck.is_none() {
                    self.write_session(cx);
                }
            }
            Plan::Wait => self.schedule_autosave(cx),
        }
    }

    /// Line the pair up on the left if "Nothing" was chosen (Swap, a
    /// version just made).
    pub(crate) fn line_up_pair(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.links.loaded() else { return };
        if !self.links.shows_left() {
            let name = file_name(&p.path);
            self.choose_left(Some(name), cx);
        }
    }

    /// The toolbar's words for the pair, while it is lined up on the left.
    pub(crate) fn pair_header(&self) -> Option<PairHeader> {
        let p = self.links.loaded().filter(|_| self.links.shows_left())?;
        let other = p.other_index();
        Some(PairHeader { left: p.links.label(other), right: p.links.label(p.this), left_hidden: self.left_hidden })
    }

    /// Why File › New version… can't make a version of this script, said
    /// before the sheet opens: one version per script for now. (A links file
    /// that can't be used is left to `version_new`, which names it.)
    pub(crate) fn new_version_refusal(&self) -> Option<String> {
        let p = self.links.loaded()?;
        let script = file_name(&self.script_path);
        let other = p.other_index();
        Some(if p.this_is_version() {
            format!("{script} is a version of {}. One version per script for now.", p.links.members[other].file)
        } else {
            format!("{script} already has a version ({}). One version per script for now.", p.links.label(other))
        })
    }

    /// Swap: the file lined up on the left, to open in this one's place.
    pub(crate) fn swap_target(&self) -> Option<PathBuf> {
        let p = self.links.loaded().filter(|_| self.links.shows_left())?;
        Some(p.other_path.clone())
    }

    /// Edit › Mark in sync (`sync`) or Mark not in sync, for the row of the line
    /// the card shows (the card focused) or else the line at the cursor.
    pub(crate) fn mark_row(&mut self, sync: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.retired {
            return;
        }
        self.commit_rewrite_quietly(cx);
        let what = if sync { "Not marked in sync" } else { "Not marked not in sync" };
        let Some(links_file) = self.links.loaded().map(|p| file_name(&p.path)) else {
            let why = match self.links.pair() {
                PairState::Unusable { reason, .. } => format!("{what}: the links file can't be used ({reason})."),
                _ => format!(
                    "Nothing is lined up with {}: File › New version… ({}) makes a version to line up.",
                    file_name(&self.script_path),
                    crate::menus::shortcut("t")
                ),
            };
            self.say("");
            self.say_error(why);
            cx.notify();
            return;
        };
        let doc = self.editor.read(cx).doc.clone();
        let para = match self.current() {
            Some((r, i)) if self.focus.is_focused(window) => anchor_para(&self.session, &doc, r, i),
            _ => self.editor.read(cx).sel.head.para,
        };
        // The rows as the document reads now (typing may not have settled).
        self.links_task = None;
        self.links.lay_out(&self.fdx, &self.session, &doc);
        let Some(row) = self.links.rows().row_of(para) else {
            self.say("");
            self.say_error(format!(
                "{what}: the line at the cursor isn't on a row of {links_file} yet (Write .fdx lists it)."
            ));
            cx.notify();
            return;
        };
        let pair = self.links.loaded().expect("checked above");
        let edit = if sync { RowEdit::Sync } else { RowEdit::Unsync };
        let mut notes = vec![];
        // The UI thread never waits for the links lock.
        let done = edit_row(pair, &self.fdx, &self.session, &doc, row + 1, edit, Duration::ZERO, &mut notes);
        match done {
            Ok(done) => {
                let n = done.row;
                let label = scriptorium_tools::LinksCtx::from(pair.view(&self.fdx, &self.session, &doc))
                    .status_label(done.status);
                let said = match (sync, done.status) {
                    (true, _) => format!("Marked row {n} in sync (Edit › Mark not in sync takes it back)."),
                    (false, RowStatus::InSync) => format!(
                        "Marked row {n} not in sync; it still reads in sync (a decided suggestion says so, or \
                         both sides are blank)."
                    ),
                    (false, _) => format!("Marked row {n} not in sync: it reads {label}."),
                };
                notes.insert(0, said);
                self.say(notes.join("  "));
                self.reload_pair_now(cx);
            }
            Err(e) => {
                crate::log::warn(&format!("marking a row: {e}"));
                self.say("");
                self.say_error(format!("{what}. {e}"));
            }
        }
        cx.notify();
    }

    /// The links file changed here: load the pair again now (the reload
    /// re-places `ROW:` suggestions, so it needs the session lock and no
    /// rewrite in progress), or else on the next look at the files.
    fn reload_pair_now(&mut self, cx: &mut Context<Self>) {
        self.links.recheck_soon();
        if self.rewriting.is_some() || self.stuck.is_some() {
            return;
        }
        let Some(_lock) = self.lock_session(Duration::ZERO) else { return };
        if self.links.due() {
            self.reload(cx);
        }
    }

    /// File › New version…: make a version of this script (the original)
    /// beside it, as `scriptorium version new` does, under this script's
    /// session lock, after saving the session (the version is made from the
    /// files on disk). Refused while the script reads otherwise here than in
    /// its file. The caller opens the version.
    pub(crate) fn make_version(
        &mut self,
        lang: &str,
        from: Option<&str>,
        name: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<NewVersion, String> {
        if self.retired {
            return Err("This script is being reopened; try again.".into());
        }
        self.commit_rewrite_quietly(cx);
        let script = file_name(&self.script_path);
        check_version_args(&script, lang, from, name).map_err(|e| sheet_words(&e))?;
        let unwritten = |this: &Self, cx: &mut Context<Self>| {
            this.unwritten_changes(cx).map(|_| {
                format!(
                    "{script} has changes in Scriptorium that aren't in the file, and a version is made from the \
                     file. Undo them, or bring them into {script} in Final Draft (Write .fdx writes {}), then try \
                     again.",
                    file_name(&super::output_path(&this.script_path))
                )
            })
        };
        if let Some(why) = unwritten(self, cx) {
            return Err(why);
        }
        let Some(_lock) = self.lock_session(scriptorium_fsio::LOCK_WAIT) else {
            return Err(format!("{script}'s files are busy in another program; try again."));
        };
        if self.disk_plan().1 == Plan::Reload {
            self.reload(cx);
            if let Some(why) = unwritten(self, cx) {
                return Err(why);
            }
        }
        if let Some(why) = &self.stuck {
            return Err(format!("The session file can't be used ({why})."));
        }
        if !self.write_session(cx) {
            return Err("The session couldn't be saved first.".into());
        }
        let mut notes = vec![];
        let made = versions::version_new(&self.script_path, lang, from, name, &mut notes);
        self.after_new_version(notes, cx);
        made.map_err(|e| sheet_words(&e))
    }

    /// The sidebar's "Lined up on the left" section: the pair (or a links
    /// file that can't be used, greyed, with the fix) and "Nothing".
    pub(super) fn pair_section(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![div().h(px(8.)).into_any_element(), section_label("Lined up on the left")];
        let shows = self.links.shows_left();
        match self.links.pair() {
            PairState::Loaded(p) => {
                let other = p.other_index();
                let name = format!("\u{25e7} {}", p.links.label(other));
                let file: SharedString = p.links.members[other].file.clone().into();
                let links_name = file_name(&p.path);
                // The language on one line, the count under it; the file in
                // the tooltip.
                out.push(
                    div()
                        .id("left-pair")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|s| s.bg(ACCENT_SOFT))
                        .when(shows, |d| d.bg(ACCENT_SOFT).text_color(ACCENT).font_weight(FontWeight::SEMIBOLD))
                        .child(div().truncate().child(name))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(INK_3)
                                .child(self.pair_count()),
                        )
                        .tooltip(move |_, cx| cx.new(|_| RailTip(file.clone())).into())
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_left(Some(links_name.clone()), cx)))
                        .into_any_element(),
                );
                if shows {
                    out.push(legend(&p.links));
                }
            }
            PairState::Unusable { links, reason, .. } => {
                let name = links.as_deref().map_or("The links file".to_string(), file_name);
                out.push(
                    div()
                        .px_2()
                        .py_1()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .text_color(INK_3)
                        .child(div().truncate().child(format!("\u{25e7} {name}")))
                        .child(div().text_size(px(11.)).child(format!("Can't be used: {reason}")))
                        .into_any_element(),
                );
            }
            PairState::None => {}
        }
        let none = matches!(self.links.pair(), PairState::None);
        out.push(
            view_row("left-none", "Nothing", String::new(), !shows)
                .on_click(cx.listener(|this, _, _, cx| this.choose_left(Some(NOTHING_ON_THE_LEFT.into()), cx)))
                .into_any_element(),
        );
        if none {
            out.push(
                div()
                    .px_2()
                    .text_color(INK_3)
                    .text_size(px(12.))
                    .child(format!("File › New version… ({}) makes one.", crate::menus::shortcut("t")))
                    .into_any_element(),
            );
        }
        out
    }

    /// The pair's count in the sidebar: on the version, the rows that need
    /// a suggestion; on the original, the rows not yet translated or
    /// changed in it.
    fn pair_count(&self) -> String {
        let Some(p) = self.links.loaded() else { return String::new() };
        let rows = self.links.rows();
        let review = if p.links.same_language() { "to review" } else { "to translate" };
        let n = if p.this_is_version() {
            rows.rows.iter().filter(|r| r.needs).count()
        } else {
            let todo = |s: RowStatus| matches!(s, RowStatus::Blank | RowStatus::OtherChanged | RowStatus::BothChanged);
            rows.rows.iter().filter(|r| todo(r.status)).count()
        };
        if rows.rows.is_empty() {
            String::new()
        } else if n == 0 {
            "in sync".into()
        } else {
            format!("{} {review}", thousands(n))
        }
    }

    /// The rail's button for the pair: lined up on the left or not; a
    /// click switches.
    pub(super) fn pair_rail(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.links.loaded()?;
        let on = self.links.shows_left();
        let other = p.other_index();
        let what = format!("{} \u{b7} {}", p.links.label(other), p.links.members[other].file);
        let tip: SharedString = if on {
            format!("Lined up on the left: {what} (click: Nothing)").into()
        } else {
            format!("Line up {what} on the left").into()
        };
        let links_name = file_name(&p.path);
        Some(
            div()
                .id("rail-left")
                .mt_2()
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .text_color(INK_2)
                .hover(|s| s.bg(ACCENT_SOFT))
                .when(on, |d| d.bg(ACCENT_SOFT).text_color(ACCENT).font_weight(FontWeight::SEMIBOLD))
                .child("\u{25e7}")
                .tooltip(move |_, cx| cx.new(|_| RailTip(tip.clone())).into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let choice = if on { NOTHING_ON_THE_LEFT.to_string() } else { links_name.clone() };
                    this.choose_left(Some(choice), cx)
                }))
                .into_any_element(),
        )
    }
}

/// The column's colours, one small line under the pair while it is on the
/// left: a swatch and a short label per status (`elements::STATUS_LEGEND`):
/// a language alone means "changed in it since the row was last in sync"
/// (the tooltip says so in full).
fn legend(links: &scriptorium_versions::Links) -> AnyElement {
    let (o, v) = links.sides();
    let mut full = vec![];
    let items: Vec<_> = STATUS_LEGEND
        .iter()
        .filter_map(|&st| {
            let look = status_look(st)?;
            let (short, long) = match st {
                RowStatus::ThisChanged => (links.label(v), format!("{} changed", links.label(v))),
                RowStatus::OtherChanged => (links.label(o), format!("{} changed", links.label(o))),
                RowStatus::BothChanged => ("both".to_string(), look.label.to_string()),
                RowStatus::Blank if links.same_language() => ("blank".into(), "blank".into()),
                RowStatus::Blank => ("to translate".into(), look.label.to_string()),
                _ => (look.label.to_string(), look.label.to_string()),
            };
            full.push(long);
            let swatch = div()
                .flex_none()
                .w(px(4.))
                .h(px(10.))
                .rounded_sm()
                .border_1()
                .border_color(look.color)
                .when(!look.outline, |d| d.bg(look.second.unwrap_or(look.color)));
            Some(div().flex_none().flex().items_center().gap(px(3.)).child(swatch).child(short))
        })
        .collect();
    let tip: SharedString = format!("Row colours: {} (since the row was last in sync).", full.join(", ")).into();
    div()
        .id("left-legend")
        .px_2()
        .flex()
        .gap(px(6.))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(10.))
        .text_color(INK_2)
        .children(items)
        .tooltip(move |_, cx| cx.new(|_| RailTip(tip.clone())).into())
        .into_any_element()
}

/// Words for the sheet: `version_new`'s messages name the CLI's flags.
fn sheet_words(e: &str) -> String {
    e.replace("--lang", "Language").replace("--from", "Original's language").replace(" -o ", " Saved as ")
}

/// `n` with thousands separated by commas.
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_view::Opened;
    use gpui::{TestAppContext, WindowHandle};
    use scriptorium_core::{ParaId, Pos, Selection};
    use scriptorium_review::sessionfile::SessionFile;

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
  </Content>
</FinalDraft>
"#;

    /// A folder with `film.fdx` and (`pair`) its version `film.es.fdx`;
    /// removed on drop.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str, pair: bool) -> Self {
            let dir = std::env::temp_dir().join(format!("scriptorium-app-pair-ui-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = dir.canonicalize().unwrap();
            std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
            if pair {
                versions::version_new(&dir.join("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
            }
            Self(dir)
        }
        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.0.join(name)).unwrap()
        }
        fn open(&self, cx: &mut TestAppContext, name: &str) -> WindowHandle<ScriptView> {
            cx.update(gpui_base::init);
            let opened = Opened::load(&self.0.join(name), &[]).unwrap();
            cx.add_window(|window, cx| ScriptView::new(opened, window, cx))
        }
        fn saved_left(&self, name: &str) -> Option<String> {
            SessionFile::from_json(&self.read(&format!("{name}.scriptorium.json"))).unwrap().links
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The choice is saved the moment it changes (a reload reads it back
    /// from the session file), and a reload after that takes the file's.
    #[gpui::test]
    fn the_left_choice_is_saved_at_once(cx: &mut TestAppContext) {
        let f = Folder::new("choice", true);
        let w = f.open(cx, "film.es.fdx");
        w.update(cx, |v, _, cx| {
            assert!(v.pair_header().is_some());
            assert_eq!(v.swap_target(), Some(f.0.join("film.fdx")));
            v.choose_left(Some(NOTHING_ON_THE_LEFT.into()), cx);
            assert!(v.pair_header().is_none() && v.swap_target().is_none());
        })
        .unwrap();
        assert_eq!(f.saved_left("film.es").as_deref(), Some("none"));
        // Another program writes the session with the pair chosen: a reload
        // takes it, since nothing chosen here is unsaved.
        let path = f.0.join("film.es.scriptorium.json");
        let json = f.read("film.es.scriptorium.json").replace("\"none\"", "\"film.es.links.txt\"");
        std::fs::write(&path, json).unwrap();
        w.update(cx, |v, _, cx| {
            let _lock = v.lock_session(Duration::ZERO).unwrap();
            v.reload(cx);
            assert_eq!(v.links.left_choice(), Some("film.es.links.txt"));
            assert!(v.links.shows_left());
            // Swap's other side: lining up is a no-op when shown already.
            v.line_up_pair(cx);
        })
        .unwrap();
        assert_eq!(f.saved_left("film.es").as_deref(), Some("film.es.links.txt"));
    }

    fn text_of(links: &str, line: &str) -> bool {
        links.lines().any(|l| l == line)
    }

    /// Edit › Mark in sync and Not in sync on the line at the cursor: the
    /// links file's stamps, not the undo stack; refused for a line the links
    /// file doesn't list.
    #[gpui::test]
    fn mark_in_sync_writes_the_links_file(cx: &mut TestAppContext) {
        let f = Folder::new("mark", true);
        let w = f.open(cx, "film.es.fdx");
        w.update(cx, |v, window, cx| {
            v.editor.update(cx, |e, cx| {
                e.run(cx, |doc, _| doc.insert_text(Selection::cursor(Pos::new(ParaId(1), 0)), "Laura sonríe."))
            });
            let undo = v.doc(cx).log().len();
            v.mark_row(true, window, cx);
            assert_eq!(v.error, SharedString::default(), "{}", v.error);
            assert!(v.notice.starts_with("Marked row 2 in sync"), "{}", v.notice);
            assert_eq!(v.doc(cx).log().len(), undo, "not an undo step");
        })
        .unwrap();
        let links = f.read("film.es.links.txt");
        assert!(text_of(&links, "  b1 action = Laura sonríe."), "{links}");
        assert!(text_of(&links, "  a1 action = Laura smiles."), "{links}");

        // The pair was loaded again at once: a second Mark right away works.
        w.update(cx, |v, window, cx| {
            assert!(!v.links.due());
            v.mark_row(false, window, cx);
            assert_eq!(v.error, SharedString::default(), "{}", v.error);
            assert_eq!(v.notice, "Marked row 2 not in sync: it reads English changed.");
        })
        .unwrap();
        let links = f.read("film.es.links.txt");
        assert!(text_of(&links, "  b1 action = Laura sonríe."), "the version's stamp stays: {links}");
        assert!(text_of(&links, "  a1 action"), "the original's goes: {links}");

        w.update(cx, |v, window, cx| {
            v.editor.update(cx, |e, cx| e.run(cx, |doc, _| doc.enter(Selection::cursor(Pos::new(ParaId(1), 5)))));
            v.mark_row(true, window, cx);
            assert!(v.error.contains("Write .fdx first"), "{}", v.error);
        })
        .unwrap();
    }

    /// File › New version… from the original makes the version beside it;
    /// a bad language code makes nothing.
    #[gpui::test]
    fn a_version_is_made_from_the_sheet(cx: &mut TestAppContext) {
        let f = Folder::new("make", false);
        let w = f.open(cx, "film.fdx");
        w.update(cx, |v, _, cx| {
            assert_eq!(v.new_version_refusal(), None);
            let e = v.make_version("e s", None, None, cx).unwrap_err();
            assert!(e.contains("Language"), "{e}");
            let made = v.make_version("fr", Some("en"), None, cx).unwrap();
            assert_eq!(made.version, f.0.join("film.fr.fdx"));
        })
        .unwrap();
        assert!(f.0.join("film.fr.links.txt").exists());
        let w0 = f.open(cx, "film.fdx");
        w0.update(cx, |v, _, cx| {
            let refused = v.new_version_refusal().unwrap();
            assert_eq!(refused, "film.fdx already has a version (Français). One version per script for now.");
            let e = v.make_version("de", Some("e n"), None, cx).unwrap_err();
            assert!(e.contains("Original's language") && !e.contains("--"), "{e}");
        })
        .unwrap();
        let w = f.open(cx, "film.fr.fdx");
        w.update(cx, |v, _, _| {
            assert_eq!(
                v.new_version_refusal().unwrap(),
                "film.fr.fdx is a version of film.fdx. One version per script for now."
            );
            let h = v.pair_header().unwrap();
            assert_eq!((h.left.as_str(), h.right.as_str()), ("English", "Français"));
        })
        .unwrap();
    }

    #[test]
    fn counts_read_with_commas() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(2812), "2,812");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }
}
