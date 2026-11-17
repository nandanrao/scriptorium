//! The file commands in `fsio::ops` (plan step A2), run directly over an
//! `Opened` held under the session lock, as the CLI and the agent hosts do.

use std::path::{Path, PathBuf};

use scriptorium_core::ElementKind;
use scriptorium_fsio::SessionLock;
use scriptorium_fsio::opened::Opened;
use scriptorium_fsio::ops::{self, Choice, SuggestionRef};
use scriptorium_review::Line;

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

const REVIEW: &str = "SCRIPT REVIEW v1\nScript: film.fdx\n\n---\nBEFORE: Laura smiles.\nAFTER: Laura grins.\n\
                      AFTER: Laura beams.\nWHY: livelier\n\n---\nBEFORE: I am going to town tomorrow.\n\
                      AFTER: I'm off to town tomorrow.\nWHY:\n";

/// A folder with `film.fdx` and `notes.review.txt`; removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-ops-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        std::fs::write(dir.join("film.fdx"), SCRIPT).unwrap();
        std::fs::write(dir.join("notes.review.txt"), REVIEW).unwrap();
        Self(dir)
    }

    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn script(&self) -> String {
        s(&self.at("film.fdx"))
    }

    fn review(&self) -> String {
        s(&self.at("notes.review.txt"))
    }

    /// Attach the review, as `reviews add` does.
    fn attached(name: &str) -> Self {
        let f = Self::new(name);
        let _lock = lock(&f);
        let mut o = Opened::open(&f.script(), &[&f.review()], &mut vec![]).unwrap();
        ops::attach(&mut o, &f.script()).unwrap();
        f
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn lock(f: &Folder) -> SessionLock {
    SessionLock::for_script(&f.at("film.fdx")).unwrap()
}

/// The id of the paragraph whose text is `text`.
fn para_id(f: &Folder, text: &str) -> u64 {
    let o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    o.doc.paragraphs().iter().find(|p| p.text() == text).unwrap().id.0
}

#[test]
fn attach_lists_each_review_and_saves_the_session() {
    let f = Folder::new("attach");
    let _lock = lock(&f);
    let mut o = Opened::open(&f.script(), &[&f.review()], &mut vec![]).unwrap();
    let out = ops::attach(&mut o, &f.script()).unwrap();
    assert_eq!(out, "attached notes.review.txt (2 suggestions)\n");
    let o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert_eq!(o.session.reviews.len(), 1);
}

#[test]
fn detach_removes_an_attached_review_and_refuses_one_that_isnt() {
    let f = Folder::attached("detach");
    let _lock = lock(&f);
    let err = ops::detach(&f.script(), &["other.review.txt"]).unwrap_err();
    assert!(err.contains("isn't attached"), "{err}");
    let out = ops::detach(&f.script(), &["notes.review.txt"]).unwrap();
    assert_eq!(out, "removed notes.review.txt (the script's text keeps its decisions)\n");
    let o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert!(o.session.reviews.is_empty());
}

#[test]
fn detach_with_no_session_says_nothing_is_attached() {
    let f = Folder::new("detach-none");
    let _lock = lock(&f);
    let err = ops::detach(&f.script(), &["notes.review.txt"]).unwrap_err();
    assert_eq!(err, format!("no review files attached to {}", f.script()));
}

#[test]
fn decide_accepts_keeps_rewrites_and_refuses() {
    let f = Folder::attached("decide");
    let _lock = lock(&f);
    let open = || Opened::open_one(&f.script(), None, None, false, &mut vec![]).unwrap();

    let (mut o, r) = open();
    assert_eq!(
        ops::decide(&mut o, r, SuggestionRef::Number(1), Choice::Accept(Some(2))).unwrap(),
        "#1 accepted 2: Laura beams.\n"
    );
    let (mut o, r) = open();
    let err = ops::decide(&mut o, r, SuggestionRef::Number(2), Choice::Accept(Some(3))).unwrap_err();
    assert_eq!(err, "no option 3 (1 to 1)");
    let (mut o, r) = open();
    assert_eq!(ops::decide(&mut o, r, SuggestionRef::Number(9), Choice::Keep).unwrap_err(), "no suggestion 9 (1 to 2)");
    let (mut o, r) = open();
    let out = ops::decide(&mut o, r, SuggestionRef::Number(2), Choice::Rewrite("Town, tomorrow.".into())).unwrap();
    assert!(out.starts_with("#2 ") && out.ends_with(": Town, tomorrow.\n"), "{out}");
    let (mut o, r) = open();
    let out = ops::decide(&mut o, r, SuggestionRef::Number(1), Choice::Keep).unwrap();
    assert!(out.ends_with(": Laura smiles.\n"), "{out}");
    // A one-line suggestion takes one line only.
    let (mut o, r) = open();
    let two = vec![Line::typed(ElementKind::Action, "a"), Line::typed(ElementKind::Action, "b")];
    let err = ops::decide(&mut o, r, SuggestionRef::Number(1), Choice::RewriteBlock(two)).unwrap_err();
    assert!(err.contains("for one line"), "{err}");

    // Saved: a fresh open sees the rewrite.
    let o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert!(o.doc.paragraphs().iter().any(|p| p.text() == "Town, tomorrow."));
}

#[test]
fn edit_then_undo_then_redo() {
    let f = Folder::new("edit");
    let id = para_id(&f, "Laura smiles.");
    let _lock = lock(&f);

    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert_eq!(ops::edit(&mut o, id, "Laura frowns.").unwrap(), format!("¶{id}: Laura frowns.\n"));
    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert!(ops::edit(&mut o, id, "two\nlines").unwrap_err().contains("newlines"));
    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert_eq!(ops::edit(&mut o, 999, "x").unwrap_err(), "no paragraph 999 (use list_paragraphs to find ids)");

    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert_eq!(ops::undo_redo(&mut o, false).unwrap(), format!("¶{id}: Laura smiles.\n"));
    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert_eq!(ops::undo_redo(&mut o, false).unwrap_err(), "nothing to undo");
    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert_eq!(ops::undo_redo(&mut o, true).unwrap(), format!("¶{id}: Laura frowns.\n"));
    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert_eq!(ops::undo_redo(&mut o, true).unwrap_err(), "nothing to redo");
}

#[test]
fn write_to_the_default_output_and_never_over_the_script() {
    let f = Folder::attached("write");
    let _lock = lock(&f);
    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    let out = ops::write(&mut o, &f.script(), false, None).unwrap();
    let own = f.at("film (scriptorium).fdx");
    assert_eq!(out, format!("wrote {} (0 paragraphs changed)\n", own.display()));
    // No edits: byte-identical (rule 2).
    assert_eq!(std::fs::read_to_string(&own).unwrap(), SCRIPT);

    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    let out = ops::write(&mut o, &f.script(), true, None).unwrap();
    assert_eq!(out, format!("wrote {} (2 paragraphs changed)\n", own.display()));
    // accept_all shapes only the output: the session is unchanged.
    let o = Opened::open_one(&f.script(), None, None, false, &mut vec![]).unwrap().0;
    assert!(!o.doc.paragraphs().iter().any(|p| p.text() == "Laura grins."));

    let mut o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    let err = ops::write(&mut o, &f.script(), false, Some(&f.at("film.fdx"))).unwrap_err();
    assert!(err.starts_with("refusing to overwrite the original script"), "{err}");
}

#[test]
fn agent_output_replaces_no_fdx_but_the_default_output() {
    let f = Folder::new("agent-output");
    let _lock = lock(&f);
    let o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    let own = f.at("film (scriptorium).fdx");

    // Default: the script's own output, also once it exists.
    assert_eq!(ops::agent_output(&o, &f.script(), None).unwrap(), own);
    std::fs::write(&own, SCRIPT).unwrap();
    assert_eq!(ops::agent_output(&o, &f.script(), None).unwrap(), own);
    assert_eq!(ops::agent_output(&o, &f.script(), Some("film (scriptorium).fdx")).unwrap(), own);

    // A new .fdx in the folder is fine; an existing other one is not.
    assert_eq!(ops::agent_output(&o, &f.script(), Some("copy.fdx")).unwrap(), f.at("copy.fdx"));
    std::fs::write(f.at("other.fdx"), SCRIPT).unwrap();
    let err = ops::agent_output(&o, &f.script(), Some("other.fdx")).unwrap_err();
    assert!(err.starts_with("other.fdx already exists and isn't this script's own output"), "{err}");
    // The script itself, outside the folder, or not a .fdx: refused.
    assert!(ops::agent_output(&o, &f.script(), Some("film.fdx")).is_err());
    assert!(ops::agent_output(&o, &f.script(), Some("../out.fdx")).is_err());
    assert!(ops::agent_output(&o, &f.script(), Some("out.txt")).is_err());
}

const BLOCKS: &str = "SCRIPT REVIEW v2\nScript: film.fdx\n\n---\nBEFORE:\n  CHARACTER: CESAR\n  DIALOGUE: I am going \
                      to town tomorrow.\nAFTER:\n  CHARACTER: CESAR\n  DIALOGUE: Town tomorrow.\nAFTER: [cut]\nWHY:\n";

#[test]
fn decide_a_block_takes_lines_and_refuses_plain_text() {
    let f = Folder::new("block");
    std::fs::write(f.at("blocks.review.txt"), BLOCKS).unwrap();
    let review = s(&f.at("blocks.review.txt"));
    let _lock = lock(&f);
    let mut o = Opened::open(&f.script(), &[&review], &mut vec![]).unwrap();
    ops::attach(&mut o, &f.script()).unwrap();
    let open = || Opened::open_one(&f.script(), None, None, false, &mut vec![]).unwrap();

    let (mut o, r) = open();
    assert!(o.session.item(r, 0).block, "the fixture is a block");
    let err = ops::decide(&mut o, r, SuggestionRef::Number(1), Choice::Rewrite("one line".into())).unwrap_err();
    assert!(err.starts_with("that suggestion is a block"), "{err}");

    let (mut o, r) = open();
    let out = ops::decide(&mut o, r, SuggestionRef::Number(1), Choice::Accept(None)).unwrap();
    assert!(out.starts_with("#1 accepted"), "{out}");
    assert!(out.contains(" CHARACTER: CESAR\n") && out.ends_with(" DIALOGUE: Town tomorrow.\n"), "{out}");

    let (mut o, r) = open();
    let out = ops::decide(&mut o, r, SuggestionRef::Number(1), Choice::Accept(Some(2))).unwrap();
    assert!(out.ends_with(":\n  [cut]\n"), "{out}");

    let (mut o, r) = open();
    let lines = vec![Line::typed(ElementKind::Action, "Cesar leaves.")];
    let out = ops::decide(&mut o, r, SuggestionRef::Number(1), Choice::RewriteBlock(lines)).unwrap();
    assert!(out.ends_with(" ACTION: Cesar leaves.\n"), "{out}");
}

#[test]
fn decide_by_entry() {
    let f = Folder::attached("entry");
    let _lock = lock(&f);
    let (mut o, r) = Opened::open_one(&f.script(), None, None, false, &mut vec![]).unwrap();
    let second = o.session.reviews[r].items.iter().map(|i| i.entry).max().unwrap();
    let out = ops::decide(&mut o, r, SuggestionRef::Entry(second), Choice::Keep).unwrap();
    assert_eq!(out, "#2 kept: I am going to town tomorrow.\n");
    let (mut o, r) = Opened::open_one(&f.script(), None, None, false, &mut vec![]).unwrap();
    let err = ops::decide(&mut o, r, SuggestionRef::Entry(99), Choice::Keep).unwrap_err();
    assert_eq!(err, "no suggestion e99 in notes.review.txt (it isn't in the file, or didn't match a line)");
}

#[test]
fn detach_by_absolute_path() {
    let f = Folder::attached("detach-abs");
    let _lock = lock(&f);
    let out = ops::detach(&f.script(), &[&f.review()]).unwrap();
    assert_eq!(out, format!("removed {} (the script's text keeps its decisions)\n", f.review()));
    let o = Opened::open(&f.script(), &[], &mut vec![]).unwrap();
    assert!(o.session.reviews.is_empty());
}

/// `film.es.fdx`, the version Scriptorium made of `film.fdx`.
fn made(name: &str) -> (Folder, String) {
    let f = Folder::new(name);
    scriptorium_fsio::versions::version_new(&f.at("film.fdx"), "es", Some("en"), None, &mut vec![]).unwrap();
    let es = s(&f.at("film.es.fdx"));
    (f, es)
}

#[test]
fn the_made_version_needs_a_named_output() {
    let (f, es) = made("made");
    let _lock = SessionLock::for_script(&f.at("film.es.fdx")).unwrap();
    let mut o = Opened::open(&es, &[], &mut vec![]).unwrap();
    assert!(o.pair().is_made());
    let err = ops::write(&mut o, &es, false, None).unwrap_err();
    assert_eq!(err, "film.es.fdx is written from the app: Write .fdx (or give -o <file>)");
    let err = ops::agent_output(&o, &es, None).unwrap_err();
    assert!(err.starts_with("film.es.fdx is the version Scriptorium made"), "{err}");
    assert!(!f.at("film.es (scriptorium).fdx").exists());
    // Named: allowed.
    let out = ops::agent_output(&o, &es, Some("copy.fdx")).unwrap();
    assert!(ops::write(&mut o, &es, false, Some(&out)).unwrap().starts_with("wrote "));
}

#[test]
fn a_script_that_may_be_the_made_version_needs_a_named_output() {
    let (f, es) = made("maybe-made");
    // A links file naming it that can't be read.
    let at = f.at("film.es.links.txt");
    let text = std::fs::read_to_string(&at).unwrap();
    std::fs::write(&at, text.replacen("1 | 1", "0 | 1", 1)).unwrap();
    let _lock = SessionLock::for_script(&f.at("film.es.fdx")).unwrap();
    let mut o = Opened::open(&es, &[], &mut vec![]).unwrap();
    assert!(o.pair().maybe_made() && !o.pair().is_made());
    let err = ops::write(&mut o, &es, false, None).unwrap_err();
    assert!(err.starts_with("film.es.fdx may be the version Scriptorium made"), "{err}");
    let err = ops::agent_output(&o, &es, None).unwrap_err();
    assert!(err.starts_with("film.es.fdx is the version Scriptorium made"), "{err}");
    assert!(!f.at("film.es (scriptorium).fdx").exists());
}
