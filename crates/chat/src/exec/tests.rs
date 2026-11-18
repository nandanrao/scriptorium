//! The apply path over an in-memory store: no disk, as the app's live store
//! has none of the CLI's files to go through.

use std::collections::BTreeMap;

use scriptorium_agent::file::ThreadFile;
use scriptorium_agent::{Thread, ToolCall};
use scriptorium_core::Script as Doc;
use scriptorium_review::Review;
use serde_json::json;

use super::*;

const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>EXT. FARM HOUSE - NIGHT</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow.</Text></Paragraph>
  </Content>
</FinalDraft>
"#;

/// A script with its "files" in a map.
struct Mem {
    dir: PathBuf,
    original: Doc,
    doc: Document,
    session: Session,
    files: BTreeMap<PathBuf, String>,
    caps: Box<dyn Fn(&ElementKind) -> bool>,
    /// A name another program creates just before this store tries to.
    race: Option<String>,
    reloads: usize,
    /// The lines being rewritten.
    editing: Vec<ParaId>,
}

impl Mem {
    fn new() -> Self {
        let original = scriptorium_fdx::FdxFile::parse(SCRIPT).unwrap().to_script();
        Mem {
            dir: PathBuf::from("/mem"),
            doc: Document::new(original.clone()),
            original,
            session: Session::new(vec![]),
            files: BTreeMap::new(),
            caps: Box::new(|k| k.name() == "Character"),
            race: None,
            reloads: 0,
            editing: vec![],
        }
    }

    fn open(&self, path: &Path) -> Review {
        let stored = stored_path(&self.dir, path);
        let mut review = Review::open_live(
            stored.clone(),
            &self.files[path],
            &self.original,
            &self.doc,
            &self.caps,
            "film.fdx",
            None,
        )
        .with_path(stored);
        review.pin_id();
        review
    }

    fn file(&self, name: &str) -> &str {
        &self.files[&self.dir.join(name)]
    }
}

impl ReviewStore for Mem {
    fn dir(&self) -> &Path {
        &self.dir
    }
    fn script_name(&self) -> &str {
        "film.fdx"
    }
    fn doc(&self) -> &Document {
        &self.doc
    }
    fn session(&self) -> &Session {
        &self.session
    }
    fn is_caps(&self) -> &dyn Fn(&ElementKind) -> bool {
        &*self.caps
    }
    fn session_mut(&mut self) -> (&mut Session, &Document, &dyn Fn(&ElementKind) -> bool) {
        (&mut self.session, &self.doc, &*self.caps)
    }
    fn disk_text(&self, path: &Path) -> Option<String> {
        self.files.get(path).cloned()
    }
    fn editing_paras(&self) -> Vec<ParaId> {
        self.editing.clone()
    }
    fn may_write(&self, path: &Path) -> Result<(), ReviewError> {
        // The files are in the map: the folder rule against the disk when
        // the folder is real (lexically under `/mem`), the text from the map.
        if self.dir.exists() {
            scriptorium_fsio::agent_write_place(&self.dir, path).map_err(ReviewError::Failed)?;
        } else if !path.starts_with(&self.dir) {
            return Err(ReviewError::Failed("outside".into()));
        }
        match self.files.get(path) {
            Some(text) if scriptorium_review::file::is_review_text(text) => Ok(()),
            _ => Err(ReviewError::Failed(format!("{} doesn't start with SCRIPT REVIEW", path.display()))),
        }
    }
    fn file_names(&self) -> Result<Vec<String>, ReviewError> {
        Ok(self.files.keys().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect())
    }
    fn reload(&mut self) -> Result<(), ReviewError> {
        self.reloads += 1;
        let paths: Vec<PathBuf> = self.session.reviews.iter().map(|r| resolve_path(&self.dir, &r.path)).collect();
        let reviews = paths.iter().map(|p| self.open(p)).collect();
        self.session = Session::new(reviews);
        Ok(())
    }
    fn attach(&mut self, path: &Path) -> Result<usize, ReviewError> {
        let review = self.open(path);
        self.session.add(review).map_err(ReviewError::Failed)
    }
    fn create(&mut self, path: &Path, text: &str) -> Result<usize, ReviewError> {
        if let Some(name) = self.race.take() {
            self.files.insert(self.dir.join(name), "someone else's".into());
        }
        if self.files.contains_key(path) {
            return Err(ReviewError::Exists);
        }
        self.files.insert(path.to_path_buf(), text.to_string());
        self.attach(path)
    }
    fn write(&mut self, r: usize, text: String) -> Result<(), ReviewError> {
        let path = resolve_path(&self.dir, &self.session.reviews[r].path);
        self.files.insert(path, text);
        Ok(())
    }
}

/// One `Script` over a `Mem`: every opening is the same store.
struct OneStore(Mem);

impl Script for OneStore {
    fn open(&mut self, _writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
        Ok(Box::new(Borrowed(&mut self.0)))
    }
}

struct Borrowed<'a>(&'a mut Mem);

impl ReviewStore for Borrowed<'_> {
    fn dir(&self) -> &Path {
        self.0.dir()
    }
    fn script_name(&self) -> &str {
        self.0.script_name()
    }
    fn doc(&self) -> &Document {
        self.0.doc()
    }
    fn session(&self) -> &Session {
        self.0.session()
    }
    fn is_caps(&self) -> &dyn Fn(&ElementKind) -> bool {
        self.0.is_caps()
    }
    fn session_mut(&mut self) -> (&mut Session, &Document, &dyn Fn(&ElementKind) -> bool) {
        self.0.session_mut()
    }
    fn disk_text(&self, path: &Path) -> Option<String> {
        self.0.disk_text(path)
    }
    fn editing_paras(&self) -> Vec<ParaId> {
        self.0.editing_paras()
    }
    fn may_write(&self, path: &Path) -> Result<(), ReviewError> {
        self.0.may_write(path)
    }
    fn file_names(&self) -> Result<Vec<String>, ReviewError> {
        self.0.file_names()
    }
    fn reload(&mut self) -> Result<(), ReviewError> {
        self.0.reload()
    }
    fn attach(&mut self, path: &Path) -> Result<usize, ReviewError> {
        self.0.attach(path)
    }
    fn create(&mut self, path: &Path, text: &str) -> Result<usize, ReviewError> {
        self.0.create(path, text)
    }
    fn write(&mut self, r: usize, text: String) -> Result<(), ReviewError> {
        self.0.write(r, text)
    }
}

fn thread(title: &str) -> ThreadFile {
    ThreadFile {
        version: 1,
        id: "t0001".into(),
        title: title.into(),
        created: String::new(),
        updated: String::new(),
        account: "work".into(),
        model: "m".into(),
        review_id: None,
        review_path: None,
        whole_script: None,
        system: String::new(),
        tools_hash: String::new(),
        thread: Thread::default(),
        extra: Default::default(),
    }
}

fn suggest(id: &str, para: u64, option: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "suggest_change".into(),
        input: json!({"paragraph_id": para, "options": [option]}),
        input_error: None,
        raw_input: None,
        extra: None,
    }
}

fn call(script: &mut OneStore, file: &mut ThreadFile, c: ToolCall) -> CallDone {
    run_call(script, Caller::Thread(file), &c)
}

#[test]
fn the_first_suggestion_creates_the_threads_review() {
    let mut s = OneStore(Mem::new());
    let mut file = thread("Dialogue pass");
    let done = call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    assert!(!done.result.is_error, "{}", done.result.content);
    assert!(done.result.content.starts_with("added suggestion #1"), "{}", done.result.content);
    assert_eq!(file.review_path.as_deref(), Some("Dialogue pass.review.txt"));
    assert_eq!(file.review_id.as_deref(), Some("thread-t0001"));
    let text = s.0.file("Dialogue pass.review.txt");
    assert!(text.contains("Id: thread-t0001") && text.contains("Laura grins."), "{text}");
    assert_eq!(done.added.unwrap().numbers, [Some(1)]);
}

#[test]
fn a_name_taken_meanwhile_moves_to_the_next() {
    let mut mem = Mem::new();
    mem.race = Some("Dialogue pass.review.txt".into());
    let mut s = OneStore(mem);
    let mut file = thread("Dialogue pass");
    let done = call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    assert!(!done.result.is_error, "{}", done.result.content);
    assert_eq!(file.review_path.as_deref(), Some("Dialogue pass 2.review.txt"));
    assert_eq!(s.0.file("Dialogue pass.review.txt"), "someone else's");
}

#[test]
fn a_detached_review_is_reattached() {
    let mut s = OneStore(Mem::new());
    let mut file = thread("Dialogue pass");
    call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    s.0.session = Session::new(vec![]);
    // Unattached: nothing to default to, so list_suggestions can't pick one.
    let listed =
        call(&mut s, &mut file, ToolCall { name: "list_suggestions".into(), input: json!({}), ..suggest("l", 0, "") });
    assert!(listed.result.is_error, "{}", listed.result.content);
    let done = call(&mut s, &mut file, suggest("b", 3, "I am going to town tomorrow."));
    assert!(!done.result.is_error, "{}", done.result.content);
    assert_eq!(s.0.files.len(), 1);
    assert_eq!(s.0.session.reviews.len(), 1);
    let text = s.0.file("Dialogue pass.review.txt");
    assert!(text.contains("Laura grins.") && text.contains("going to town"), "{text}");
    // Attached again, list_suggestions defaults to it.
    let listed =
        call(&mut s, &mut file, ToolCall { name: "list_suggestions".into(), input: json!({}), ..suggest("l", 0, "") });
    assert!(listed.result.content.contains("Laura grins."), "{}", listed.result.content);
}

#[test]
fn a_review_changed_on_disk_is_reloaded_first() {
    let mut s = OneStore(Mem::new());
    let mut file = thread("Dialogue pass");
    call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    // Another program adds a suggestion on the dialogue line to the file.
    let path = s.0.dir.join("Dialogue pass.review.txt");
    let more = format!(
        "{}\n---\nPARA: 3\nBEFORE: I am go to the town tomorrow.\nAFTER: I go to town tomorrow.\n",
        s.0.files[&path]
    );
    s.0.files.insert(path.clone(), more);
    let done = call(&mut s, &mut file, suggest("b", 3, "I am going to town tomorrow."));
    assert_eq!(s.0.reloads, 1);
    // The other program's suggestion is kept, and this one is refused as a duplicate.
    assert!(done.result.is_error, "{}", done.result.content);
    assert!(done.result.content.contains("this conversation's review"), "{}", done.result.content);
    assert!(s.0.files[&path].contains("I go to town tomorrow."));
}

#[test]
fn whole_script_rule() {
    let price = Price { input: 5.0, output: 25.0, cache_read: None, cache_write: None, above: None };
    assert_eq!(gate_for(1_000, Some(&price)), Gate::Run);
    assert!(matches!(gate_for(20_001, None), Gate::Ask { tokens: 20_001, cost: None }));
    // $5 a million: 20k tokens is $0.10, just at the line.
    assert_eq!(gate_for(20_000, Some(&price)), Gate::Run);
    let pricey = Price { input: 15.0, ..price };
    assert!(matches!(gate_for(10_000, Some(&pricey)), Gate::Ask { .. }));
    assert_eq!(
        whole_script_question(35_000, Some(0.18)),
        "The agent wants to read the whole script (about 35k tokens, ≈ $0.18)."
    );
    assert_eq!(whole_script_question(800, None), "The agent wants to read the whole script (about 800 tokens).");

    let mut s = OneStore(Mem::new());
    let mut file = thread("t");
    assert_eq!(whole_script_gate(&mut s, &file, "read_scene", false, None).unwrap(), Gate::Run);
    assert_eq!(whole_script_gate(&mut s, &file, "read_script", false, None).unwrap(), Gate::Run);
    file.whole_script = Some(false);
    assert_eq!(whole_script_gate(&mut s, &file, "read_script", false, None).unwrap(), Gate::Refuse);
    assert_eq!(whole_script_gate(&mut s, &file, "read_script", true, None).unwrap(), Gate::Run);
}

#[test]
fn a_crash_between_create_and_save_reattaches_by_id() {
    let mut s = OneStore(Mem::new());
    let mut file = thread("Dialogue pass");
    call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    // The file was created, but neither the thread nor the session was saved.
    file.review_path = None;
    file.review_id = None;
    s.0.session = Session::new(vec![]);
    let done = call(&mut s, &mut file, suggest("b", 3, "I am going to town tomorrow."));
    assert!(!done.result.is_error, "{}", done.result.content);
    assert_eq!(s.0.files.len(), 1, "{:?}", s.0.files.keys());
    assert_eq!(file.review_path.as_deref(), Some("Dialogue pass.review.txt"));
    let text = s.0.file("Dialogue pass.review.txt");
    assert!(text.contains("Laura grins.") && text.contains("going to town"), "{text}");
}

#[test]
fn a_planned_path_is_used_but_never_claims_another_file() {
    // Planned and free: created there.
    let mut s = OneStore(Mem::new());
    let mut file = thread("Dialogue pass");
    file.review_path = Some("Planned.review.txt".into());
    let done = call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    assert!(!done.result.is_error, "{}", done.result.content);
    assert!(s.0.file("Planned.review.txt").contains("Id: thread-t0001"));
    assert_eq!(file.review_id.as_deref(), Some("thread-t0001"));

    // Planned, but another program put a file of its own there first.
    let mut s = OneStore(Mem::new());
    s.0.files.insert(s.0.dir.join("Planned.review.txt"), "Id: someone-else\n".into());
    let mut file = thread("Dialogue pass");
    file.review_path = Some("Planned.review.txt".into());
    let done = call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    assert!(!done.result.is_error, "{}", done.result.content);
    assert_eq!(s.0.file("Planned.review.txt"), "Id: someone-else\n");
    assert_eq!(file.review_path.as_deref(), Some("Dialogue pass.review.txt"));
}

#[test]
fn plan_review_path_names_a_free_file() {
    let dir = std::env::temp_dir().join(format!("scriptorium-plan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Dialogue pass.review.txt"), "").unwrap();
    let mut file = thread("Dialogue pass");
    assert_eq!(plan_review_path(&dir, &file).as_deref(), Some("Dialogue pass 2.review.txt"));
    file.review_path = Some("x.review.txt".into());
    assert_eq!(plan_review_path(&dir, &file), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_argument_not_offered_is_refused() {
    let mut s = OneStore(Mem::new());
    let mut file = thread("Dialogue pass");
    // The host offers no `review_file`: naming any file is refused before
    // anything is opened, so nothing is created, attached or written.
    for (path, input) in [
        (
            "notes.review.txt",
            json!({"paragraph_id": 1, "options": ["Laura grins."], "review_file": "notes.review.txt"}),
        ),
        ("/home/u/.profile", json!({"paragraph_id": 1, "options": ["x"], "review_file": "/home/u/.profile"})),
        (
            "batch item",
            json!({"suggestions": [{"paragraph_id": 1, "options": ["x"], "review_file": "../x.review.txt"}]}),
        ),
    ] {
        let name = if input.get("suggestions").is_some() { "suggest_changes" } else { "suggest_change" };
        let call = ToolCall { name: name.into(), input, ..suggest("a", 1, "x") };
        let done = run_call(&mut s, Caller::Thread(&mut file), &call);
        assert!(done.result.is_error && done.result.content.contains("review_file"), "{path}: {}", done.result.content);
        assert!(s.0.files.is_empty() && s.0.session.reviews.is_empty(), "{path}");
    }
    // A top-level review_file on the batch tool, and a name not offered.
    let call = ToolCall {
        name: "suggest_changes".into(),
        input: json!({"suggestions": [{"paragraph_id": 1, "options": ["x"]}], "review_file": "/tmp/x.review.txt"}),
        ..suggest("c", 1, "x")
    };
    let done = run_call(&mut s, Caller::Thread(&mut file), &call);
    assert!(done.result.is_error && done.result.content.contains("review_file"), "{}", done.result.content);
    let call = ToolCall { name: "write_file".into(), input: json!({"path": "/tmp/x"}), ..suggest("d", 1, "x") };
    let done = run_call(&mut s, Caller::Thread(&mut file), &call);
    assert!(done.result.is_error && done.result.content.contains("unknown tool"), "{}", done.result.content);
    assert!(s.0.files.is_empty() && s.0.session.reviews.is_empty());
    // A read tool too.
    let call = ToolCall {
        name: "list_suggestions".into(),
        input: json!({"review": "x", "path": "/etc"}),
        ..suggest("b", 1, "x")
    };
    let done = run_call(&mut s, Caller::Thread(&mut file), &call);
    assert!(done.result.is_error && done.result.content.contains("`path`"), "{}", done.result.content);
    assert_eq!(file.review_path, None);
}

/// Suggestions for `path` as a [`Target::File`], through the apply path
/// (what `review_file` would give if a host passed one on).
fn to_file(m: &mut Mem, file: &mut ThreadFile, path: &str) -> Result<Applied, ReviewError> {
    let append = {
        let ctx = Ctx {
            doc: &m.doc,
            session: &m.session,
            script_name: "film.fdx",
            is_caps: &*m.caps,
            default_review: None,
            links: None,
        };
        let args = json!({"paragraph_id": 1, "options": ["Laura grins."], "review_file": path});
        match scriptorium_tools::call(&ctx, "suggest_change", &args).unwrap() {
            Outcome::Append(a) => a,
            other => panic!("{other:?}"),
        }
    };
    apply_append(m, Caller::Thread(file), append)
}

#[test]
fn a_file_target_must_be_a_review_in_the_scripts_folder() {
    let base = std::env::temp_dir().join(format!("scriptorium-file-target-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("film")).unwrap();
    let base = base.canonicalize().unwrap();
    let mut m = Mem::new();
    m.dir = base.join("film");
    let outside = base.join("outside.review.txt");
    std::fs::write(&outside, format!("{FORMAT_HEADER}\n")).unwrap();
    m.files.insert(outside.clone(), format!("{FORMAT_HEADER}\n"));
    // Not a review, though named like one.
    m.files.insert(m.dir.join("config.review.txt"), "export PATH=/tmp\n".into());
    m.files.insert(m.dir.join("notes.txt"), format!("{FORMAT_HEADER}\n"));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, m.dir.join("link.review.txt")).unwrap();
    let before = m.files.clone();
    let mut file = thread("Dialogue pass");
    let mut refused = vec![
        outside.to_string_lossy().into_owned(),
        "../outside.review.txt".into(),
        "sub/../../outside.review.txt".into(),
        "config.review.txt".into(),
        "notes.txt".into(),
        "/home/u/.profile".into(),
    ];
    if cfg!(unix) {
        refused.push("link.review.txt".into());
    }
    for path in &refused {
        let e = to_file(&mut m, &mut file, path).unwrap_err();
        assert!(matches!(e, ReviewError::Failed(_)), "{path}: {e:?}");
        assert_eq!(m.files, before, "{path}");
        assert!(m.session.reviews.is_empty(), "{path}");
    }
    // A review in the folder: created, then attached and added to.
    to_file(&mut m, &mut file, "pass.review.txt").unwrap();
    assert!(m.file("pass.review.txt").starts_with(FORMAT_HEADER) && m.file("pass.review.txt").contains("Laura grins."));
    assert_eq!(m.session.reviews.len(), 1);
    assert_eq!(file.review_path, None);
    // A missing folder inside the script's folder is made with the file.
    to_file(&mut m, &mut file, "missing/x.review.txt").unwrap();
    assert!(m.dir.join("missing").is_dir());
    assert!(m.file("missing/x.review.txt").contains("Laura grins."));
    assert_eq!(m.session.reviews.len(), 2);
    assert_eq!(m.session.reviews[1].path, "missing/x.review.txt");
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_threads_review_path_outside_the_folder_is_never_reattached() {
    let base = std::env::temp_dir().join(format!("scriptorium-review-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("film")).unwrap();
    let base = base.canonicalize().unwrap();
    let mut m = Mem::new();
    m.dir = base.join("film");
    let outside = base.join("planted.review.txt");
    let planted = new_review_text("film.fdx", "t0001");
    std::fs::write(&outside, &planted).unwrap();
    m.files.insert(outside.clone(), planted.clone());
    let mut s = OneStore(m);
    let mut file = thread("Dialogue pass");
    file.review_id = Some("thread-t0001".into());
    for stored in ["../planted.review.txt", outside.to_str().unwrap()] {
        file.review_path = Some(stored.into());
        let done = call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
        assert!(!done.result.is_error, "{}", done.result.content);
        assert_eq!(s.0.files[&outside], planted, "{stored}");
        assert!(s.0.session.reviews.iter().all(|r| !r.path.contains("planted")), "{stored}");
        // Made afresh in the script's folder instead.
        assert_eq!(file.review_path.as_deref(), Some("Dialogue pass.review.txt"));
        s.0.session = Session::new(vec![]);
        s.0.files.retain(|p, _| *p == outside);
    }
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn an_attached_review_outside_the_folder_is_never_written() {
    // A session file (planted, or made by hand) lists a review elsewhere.
    let base = std::env::temp_dir().join(format!("scriptorium-outside-attached-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("film")).unwrap();
    let base = base.canonicalize().unwrap();
    let mut m = Mem::new();
    m.dir = base.join("film");
    let outside = base.join("planted.review.txt");
    let text = new_review_text("film.fdx", "t0001");
    std::fs::write(&outside, &text).unwrap();
    m.files.insert(outside.clone(), text.clone());
    m.attach(&outside).unwrap();
    let mut s = OneStore(m);
    let mut file = thread("Dialogue pass");
    // It is the thread's own review by id, so it is what a call would use.
    let done = call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
    assert!(
        done.result.is_error && done.result.content.contains("outside the script's folder"),
        "{}",
        done.result.content
    );
    assert_eq!(s.0.files[&outside], text);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_planted_planned_name_is_never_created() {
    // A thread file whose planned review name is no review's: a fresh name
    // is used instead, and the planted one is never created.
    for planted in ["Makefile", ".zshenv", ".x.review.txt", "run.sh"] {
        let mut s = OneStore(Mem::new());
        let mut file = thread("Dialogue pass");
        file.review_path = Some(planted.into());
        let done = call(&mut s, &mut file, suggest("a", 1, "Laura grins."));
        assert!(!done.result.is_error, "{planted}: {}", done.result.content);
        let names: Vec<_> = s.0.files.keys().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Dialogue pass.review.txt"], "{planted}");
        assert_eq!(file.review_path.as_deref(), Some("Dialogue pass.review.txt"));
    }
}

/// A script whose files are always busy.
struct Busy;

impl Script for Busy {
    fn open(&mut self, _writes: bool) -> Result<Box<dyn ReviewStore + '_>, ReviewError> {
        Err(ReviewError::Busy)
    }
}

#[test]
fn busy_is_a_flag_not_just_text() {
    let mut file = thread("t");
    let done = run_call(&mut Busy, Caller::Thread(&mut file), &suggest("a", 1, "Laura grins."));
    assert!(done.busy && done.result.is_error);
    assert_eq!(done.result.content, BUSY);
    assert_eq!(whole_script_gate(&mut Busy, &file, "read_script", false, None), Err(ReviewError::Busy));
}

#[test]
fn a_stored_answer_holds_until_the_script_doubles() {
    assert_eq!(gate_with(Some(false), Some(30_000), 59_000, None), Gate::Refuse);
    assert_eq!(gate_with(Some(true), Some(30_000), 60_000, None), Gate::Run);
    assert!(matches!(gate_with(Some(false), Some(30_000), 60_001, None), Gate::Ask { tokens: 60_001, .. }));
    assert!(matches!(gate_with(Some(true), Some(30_000), 70_000, None), Gate::Ask { .. }));
    // An answer from before sizes were stored holds.
    assert_eq!(gate_with(Some(false), None, 1_000_000, None), Gate::Refuse);

    let mut file = thread("t");
    record_whole_script(&mut file, Some(false), Some(30_000));
    assert_eq!((file.whole_script, whole_script_tokens(&file)), (Some(false), Some(30_000)));
    // The app's override keeps the size.
    let kept = whole_script_tokens(&file);
    record_whole_script(&mut file, Some(true), kept);
    assert_eq!((file.whole_script, whole_script_tokens(&file)), (Some(true), Some(30_000)));
    record_whole_script(&mut file, None, None);
    assert_eq!((file.whole_script, whole_script_tokens(&file)), (None, None));
    assert!(file.extra.is_empty());
}

#[test]
fn the_new_review_text() {
    let t = new_review_text("film.fdx", "t0007");
    assert!(t.starts_with(FORMAT_HEADER), "{t}");
    assert!(t.contains("Script: film.fdx") && t.contains("Id: thread-t0007"), "{t}");
}

#[test]
fn a_proposal_on_the_line_being_rewritten_is_refused() {
    let mut s = OneStore(Mem::new());
    // A block of two lines is being rewritten; the proposal is on its second.
    s.0.editing = vec![ParaId(2), ParaId(3)];
    let mut file = thread("Dialogue pass");
    // One suggestion, on that line: an error, nothing written.
    let done = call(&mut s, &mut file, suggest("a", 3, "I am going to town tomorrow."));
    assert!(done.result.is_error, "{}", done.result.content);
    assert!(done.result.content.contains(EDITING_LINE), "{}", done.result.content);
    // A batch: that item fails, the rest go on.
    let batch = ToolCall {
        name: "suggest_changes".into(),
        input: json!({"suggestions": [
            {"paragraph_id": 1, "options": ["Laura grins."]},
            {"paragraph_id": 3, "options": ["I am going to town tomorrow."]},
        ]}),
        ..suggest("b", 0, "")
    };
    let done = call(&mut s, &mut file, batch);
    assert!(!done.result.is_error, "{}", done.result.content);
    assert!(done.result.content.contains(EDITING_LINE), "{}", done.result.content);
    assert_eq!(done.added.unwrap().numbers, [Some(1)]);
    let text = s.0.file("Dialogue pass.review.txt");
    assert!(text.contains("Laura grins.") && !text.contains("going to town"), "{text}");
}

#[test]
fn a_new_file_review_with_nothing_written_leaves_the_session_as_it_was() {
    // The app's store keeps its session: a review added in memory for a
    // `review_file` must not stay listed when every suggestion was refused.
    let base = std::env::temp_dir().join(format!("scriptorium-file-forget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let mut m = Mem::new();
    m.dir = base.canonicalize().unwrap();
    m.editing = vec![ParaId(1)];
    let mut file = thread("Dialogue pass");
    let e = to_file(&mut m, &mut file, "sub/new.review.txt").unwrap_err();
    assert_eq!(e, ReviewError::Failed(EDITING_LINE.into()));
    assert!(m.session.reviews.is_empty());
    assert!(m.files.is_empty());
    assert!(!m.dir.join("sub").exists());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn host_operations_a_store_doesnt_offer_are_refused() {
    let mut s = OneStore(Mem::new());
    let mut file = thread("Ops");
    for (name, input) in [
        ("edit_paragraph", json!({ "paragraph_id": 1, "text": "Laura grins." })),
        ("undo", json!({})),
        ("redo", json!({})),
        ("decide_suggestion", json!({ "n": 1, "decision": "keep" })),
        ("write_script", json!({})),
        ("detach_reviews", json!({ "files": ["a.review.txt"] })),
    ] {
        let c = ToolCall { name: name.into(), input: input.clone(), ..suggest("a", 1, "x") };
        let done = run_call(&mut s, Caller::Agent, &c);
        assert!(done.result.is_error && done.result.content == format!("{name} isn't offered here"), "{name}");
        // A thread isn't offered them at all.
        let done = call(&mut s, &mut file, c);
        assert!(done.result.is_error && done.result.content.contains("unknown tool"), "{}", done.result.content);
    }
    assert_eq!(s.0.doc.paragraphs()[1].text(), "Laura smiles.");
}

#[test]
fn a_leading_tilde_is_the_home_folder() {
    assert_eq!(expand_home_with("~/Scripts/a.fdx", Some("/Users/w")), "/Users/w/Scripts/a.fdx");
    assert_eq!(expand_home_with("~", Some("/Users/w/")), "/Users/w/");
    assert_eq!(expand_home_with("~bob/a.fdx", Some("/Users/w")), "~bob/a.fdx");
    assert_eq!(expand_home_with("a/~/b", Some("/Users/w")), "a/~/b");
    assert_eq!(expand_home_with("~/a", None), "~/a");
    assert_eq!(resolve_in(Path::new("/s"), "r.review.txt"), "/s/r.review.txt");
    assert_eq!(resolve_in(Path::new("/s"), "/x/r.review.txt"), "/x/r.review.txt");
}

fn app_call(script: &mut OneStore, name: &str, input: serde_json::Value) -> CallDone {
    let c = ToolCall { name: name.into(), input, ..suggest("a", 1, "x") };
    run_call(script, Caller::App { name: "scene-list" }, &c)
}

/// A real (empty) folder for the script, so the folder rules can be checked.
fn temp_folder(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("scriptorium-exec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base.canonicalize().unwrap()
}

#[test]
fn an_apps_suggestions_go_into_its_own_review() {
    let dir = temp_folder("app-review");
    let mut m = Mem::new();
    m.dir = dir.clone();
    // Another review attached, the only one: reads default to it, but the
    // app's suggestions never go there.
    let other = dir.join("a.review.txt");
    m.files.insert(other.clone(), format!("{FORMAT_HEADER}\nScript: film.fdx\n\n"));
    m.attach(&other).unwrap();
    let mut s = OneStore(m);
    let listed = app_call(&mut s, "list_suggestions", json!({}));
    assert!(!listed.result.is_error, "{}", listed.result.content);

    let name = "scene-list (app).review.txt";
    let first = app_call(&mut s, "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]}));
    assert!(!first.result.is_error, "{}", first.result.content);
    assert_eq!(first.added.as_ref().unwrap().numbers, [Some(1)]);
    let text = s.0.file(name).to_string();
    assert!(text.starts_with(FORMAT_HEADER) && text.contains("Laura grins."), "{text}");
    assert_eq!(s.0.session.reviews.len(), 2);
    assert_eq!(s.0.session.reviews[1].path, name);
    assert_eq!(s.0.file("a.review.txt"), format!("{FORMAT_HEADER}\nScript: film.fdx\n\n"));

    // The second appends to the same file.
    let second = app_call(
        &mut s,
        "suggest_changes",
        json!({"suggestions": [{"paragraph_id": 3, "options": ["I am going to town tomorrow."]}]}),
    );
    assert!(!second.result.is_error, "{}", second.result.content);
    assert_eq!(second.added.as_ref().unwrap().numbers, [Some(2)]);
    let text = s.0.file(name);
    assert!(text.contains("Laura grins.") && text.contains("I am going to town tomorrow."), "{text}");
    assert_eq!(s.0.session.reviews.len(), 2);
    // Reads now default to the app's review.
    let listed = app_call(&mut s, "list_suggestions", json!({}));
    assert!(listed.result.content.contains("Laura grins."), "{}", listed.result.content);

    // Naming another review goes there, under the agent rules.
    let named = app_call(
        &mut s,
        "suggest_change",
        json!({"paragraph_id": 1, "options": ["Laura beams."], "review_file": "a.review.txt"}),
    );
    assert!(!named.result.is_error, "{}", named.result.content);
    assert!(s.0.file("a.review.txt").contains("Laura beams."));
    let outside = app_call(
        &mut s,
        "suggest_change",
        json!({"paragraph_id": 1, "options": ["x"], "review_file": "../x.review.txt"}),
    );
    assert!(outside.result.is_error, "{}", outside.result.content);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_app_is_offered_only_the_app_tools() {
    let mut s = OneStore(Mem::new());
    let mut refused: Vec<(&str, serde_json::Value)> = vec![
        ("edit_paragraph", json!({ "paragraph_id": 1, "text": "Laura grins." })),
        ("undo", json!({})),
        ("redo", json!({})),
        ("decide_suggestion", json!({ "n": 1, "decision": "keep" })),
        ("write_script", json!({})),
        ("attach_reviews", json!({ "files": ["a.review.txt"] })),
        ("detach_reviews", json!({ "files": ["a.review.txt"] })),
        ("guide", json!({ "topic": "app" })),
        ("new_version", json!({ "language": "es" })),
        ("open_app", json!({ "name": "scene-list" })),
        // `script` isn't offered: an app works on the open script only.
        ("script_info", json!({ "script": "/elsewhere/film.fdx" })),
        ("read_scene", json!({ "scene": 1, "script": "film.fdx" })),
    ];
    for name in scriptorium_tools::AGENT_ONLY {
        if !refused.iter().any(|(n, _)| *n == name) {
            refused.push((name, json!({})));
        }
    }
    for (name, input) in refused {
        let done = app_call(&mut s, name, input);
        assert!(done.result.is_error, "{name}: {}", done.result.content);
    }
    assert_eq!(s.0.doc.paragraphs()[1].text(), "Laura smiles.");
    assert!(s.0.files.is_empty() && s.0.session.reviews.is_empty());
}

#[test]
fn list_apps_reads_the_scripts_folder() {
    let dir = temp_folder("list-apps");
    let mut m = Mem::new();
    m.dir = dir.clone();
    let mut s = OneStore(m);
    let list = |s: &mut OneStore, caller: Caller<'_>| {
        let c = ToolCall { name: "list_apps".into(), input: json!({}), ..suggest("a", 1, "x") };
        let done = run_call(s, caller, &c);
        assert!(!done.result.is_error, "{}", done.result.content);
        done.result.content
    };
    assert_eq!(list(&mut s, Caller::Agent), format!("No apps in {}/apps/.", dir.display()));

    let app = dir.join("apps/scene-list");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("app.json"), r#"{"scriptorium": 1, "title": "Scene list", "description": "Every scene."}"#)
        .unwrap();
    std::fs::write(app.join("index.html"), "<p>hi</p>").unwrap();
    std::fs::create_dir_all(dir.join("apps/broken")).unwrap();
    std::fs::write(dir.join("apps/broken/app.json"), "{ not json").unwrap();
    for caller in [Caller::Agent, Caller::App { name: "scene-list" }] {
        let text = list(&mut s, caller);
        assert!(text.starts_with(&format!("2 apps in {}/apps/:", dir.display())), "{text}");
        assert!(text.contains("- scene-list: Scene list (Every scene.)"), "{text}");
        assert!(text.contains(&format!("folder {}/apps/scene-list/", dir.display())), "{text}");
        assert!(text.contains("- broken\n") && text.contains("problem:"), "{text}");
        // No server here: nothing said about pages.
        assert!(!text.contains("page:"), "{text}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn script_info_names_the_folder_for_agents_and_apps_only() {
    let mut s = OneStore(Mem::new());
    let c = ToolCall { name: "script_info".into(), input: json!({}), ..suggest("a", 1, "x") };
    let mut file = thread("Info");
    let chat = call(&mut s, &mut file, c.clone()).result.content;
    assert!(chat.starts_with("paragraphs "), "{chat}");
    for caller in [Caller::Agent, Caller::App { name: "scene-list" }] {
        let done = run_call(&mut s, caller, &c);
        assert!(!done.result.is_error, "{}", done.result.content);
        assert_eq!(done.result.content, format!("folder      /mem\n{chat}"));
    }
}

#[test]
fn open_app_on_the_files_says_scriptorium_isnt_running() {
    let mut s = OneStore(Mem::new());
    let c = ToolCall { name: "open_app".into(), input: json!({ "name": "scene-list" }), ..suggest("a", 1, "x") };
    let done = run_call(&mut s, Caller::Agent, &c);
    assert!(done.result.is_error);
    assert_eq!(
        done.result.content,
        "Scriptorium isn't running, so the app can't be opened. Ask the writer to open the script in Scriptorium, \
         then call open_app again."
    );
}

#[test]
fn an_app_with_a_bad_name_is_refused_before_anything() {
    let dir = temp_folder("app-bad-name");
    let mut m = Mem::new();
    m.dir = dir.clone();
    let mut s = OneStore(m);
    for name in ["a/b", "../x", "", "Scene", ".hidden"] {
        for (tool, input) in
            [("suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]})), ("script_info", json!({}))]
        {
            let c = ToolCall { name: tool.into(), input, ..suggest("a", 1, "x") };
            let done = run_call(&mut s, Caller::App { name }, &c);
            assert!(done.result.is_error, "{name}: {}", done.result.content);
            assert_eq!(done.result.content, bad_app_name(name));
        }
    }
    assert!(s.0.files.is_empty() && s.0.session.reviews.is_empty());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_existing_apps_review_is_attached_and_a_non_review_refused() {
    let dir = temp_folder("app-existing");
    let name = "scene-list (app).review.txt";
    let mut m = Mem::new();
    m.dir = dir.clone();
    let at = dir.join(name);
    let before = format!("{FORMAT_HEADER}\nScript: film.fdx\n\n");
    std::fs::write(&at, &before).unwrap();
    m.files.insert(at.clone(), before.clone());
    let mut s = OneStore(m);
    let done = app_call(&mut s, "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]}));
    assert!(!done.result.is_error, "{}", done.result.content);
    assert_eq!(s.0.session.reviews.len(), 1);
    assert_eq!(s.0.session.reviews[0].path, name);
    let text = s.0.file(name);
    assert!(text.starts_with(FORMAT_HEADER) && text.contains("Laura grins."), "{text}");

    // Not a review at that name: refused, left as it is, nothing attached.
    let mut m = Mem::new();
    m.dir = dir.clone();
    std::fs::write(&at, "export PATH=/tmp\n").unwrap();
    m.files.insert(at.clone(), "export PATH=/tmp\n".into());
    let mut s = OneStore(m);
    let done = app_call(&mut s, "suggest_change", json!({"paragraph_id": 1, "options": ["Laura grins."]}));
    assert!(done.result.is_error, "{}", done.result.content);
    assert_eq!(s.0.file(name), "export PATH=/tmp\n");
    assert!(s.0.session.reviews.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}
