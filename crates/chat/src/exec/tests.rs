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
        let mut review =
            Review::open_live(stored.clone(), &self.files[path], &self.original, &self.doc, &self.caps, "film.fdx")
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
    run_call(script, file, &c)
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
        let done = run_call(&mut s, &mut file, &call);
        assert!(done.result.is_error && done.result.content.contains("review_file"), "{path}: {}", done.result.content);
        assert!(s.0.files.is_empty() && s.0.session.reviews.is_empty(), "{path}");
    }
    // A top-level review_file on the batch tool, and a name not offered.
    let call = ToolCall {
        name: "suggest_changes".into(),
        input: json!({"suggestions": [{"paragraph_id": 1, "options": ["x"]}], "review_file": "/tmp/x.review.txt"}),
        ..suggest("c", 1, "x")
    };
    let done = run_call(&mut s, &mut file, &call);
    assert!(done.result.is_error && done.result.content.contains("review_file"), "{}", done.result.content);
    let call = ToolCall { name: "write_file".into(), input: json!({"path": "/tmp/x"}), ..suggest("d", 1, "x") };
    let done = run_call(&mut s, &mut file, &call);
    assert!(done.result.is_error && done.result.content.contains("unknown tool"), "{}", done.result.content);
    assert!(s.0.files.is_empty() && s.0.session.reviews.is_empty());
    // A read tool too.
    let call = ToolCall {
        name: "list_suggestions".into(),
        input: json!({"review": "x", "path": "/etc"}),
        ..suggest("b", 1, "x")
    };
    let done = run_call(&mut s, &mut file, &call);
    assert!(done.result.is_error && done.result.content.contains("`path`"), "{}", done.result.content);
    assert_eq!(file.review_path, None);
}

/// Suggestions for `path` as a [`Target::File`], through the apply path
/// (what `review_file` would give if a host passed one on).
fn to_file(m: &mut Mem, file: &mut ThreadFile, path: &str) -> Result<Applied, ReviewError> {
    let append = {
        let ctx =
            Ctx { doc: &m.doc, session: &m.session, script_name: "film.fdx", is_caps: &*m.caps, default_review: None };
        let args = json!({"paragraph_id": 1, "options": ["Laura grins."], "review_file": path});
        match scriptorium_tools::call(&ctx, "suggest_change", &args).unwrap() {
            Outcome::Append(a) => a,
            other => panic!("{other:?}"),
        }
    };
    apply_append(m, file, append)
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
        "missing/x.review.txt".into(),
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
    std::fs::remove_dir_all(&base).unwrap();
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
    let done = run_call(&mut Busy, &mut file, &suggest("a", 1, "Laura grins."));
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
