//! End to end (plan step A12): the library seams the hosts use, in order.
//! `new_version` through the tools → `suggest_changes` on rows (a batch with
//! a heading and a block of two lines) → decisions as the CLI makes them
//! (accept, keep, rewrite) → `write_in_place` → Final Draft edits to both
//! files, as bytes (a Spanish heading retyped, a Spanish line split, a blank
//! line translated, an English line added and one reworded, both re-saved
//! with other whitespace) → reopen (re-find) → rows: every status, every
//! decision on its row, the reworded row needs an update, suggested into the
//! same review and accepted → a second `write_in_place`. Also on the real
//! script when present (a copy in a temporary folder; never written beside it).

use std::path::{Path, PathBuf};

use scriptorium_core::ElementKind;
use scriptorium_fsio::SessionLock;
use scriptorium_fsio::opened::Opened;
use scriptorium_fsio::versions::{version_new, write_in_place};
use scriptorium_review::{Decision, Status};
use scriptorium_tools::{Ctx, LinksCtx, Outcome, Target, apply, call, find_review, uses_links};
use scriptorium_versions::links::{self, Stamp};
use scriptorium_versions::{Note, RowStatus};
use serde_json::{Value, json};

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
    <Paragraph Type="Character">
      <Text>LAURA</Text>
    </Paragraph>
    <Paragraph Type="Parenthetical">
      <Text>(quietly)</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>Take me with you.</Text>
    </Paragraph>
    <Paragraph Type="Scene Heading">
      <Text>INT. BARN - DAY</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>Hay everywhere. The cows watch.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text>CESAR</Text>
    </Paragraph>
    <Paragraph Type="Dialogue">
      <Text>We leave at dawn.</Text>
    </Paragraph>
    <Paragraph Type="Action">
      <Text>She nods.</Text>
    </Paragraph>
  </Content>
</FinalDraft>
"#;

const REVIEW: &str = "Translation from English.review.txt";

/// A temporary folder, removed on drop.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("scriptorium-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }
    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.at(name)).unwrap()
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.at(name), text).unwrap();
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

/// One tool call as the CLI's `tool` runs it: under the script's lock, the
/// script opened with its pair, `Ctx` with the pair's rows for the tools
/// that read them; a `new_version` made with `version_new`; suggestions
/// applied to the attached review `review` (with the row resolver), its
/// file written, then the session saved. Returns the tool's text.
fn tool(script: &Path, name: &str, args: Value, review: Option<&str>) -> String {
    let _lock = SessionLock::for_script(script).unwrap();
    let mut notes = vec![];
    let mut o = Opened::open(&s(script), &[], &mut notes).unwrap_or_else(|e| panic!("{e}"));
    let mut args = args;
    if let Some(r) = review {
        let r = find_review(&o.session, r).map(|i| o.session.reviews[i].path.clone()).unwrap_or(r.to_string());
        args.as_object_mut().unwrap().insert("review_file".into(), Value::String(r));
    }
    let outcome = {
        let caps = |k: &ElementKind| o.fdx.is_caps(k.name());
        let ctx = Ctx {
            doc: &o.doc,
            session: &o.session,
            script_name: &o.script_name,
            is_caps: &caps,
            default_review: (o.session.reviews.len() == 1).then_some(0),
            links: uses_links(name).then(|| o.pair_view().map(|v| v.map(LinksCtx::from))).flatten(),
        };
        call(&ctx, name, &args).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let append = match outcome {
        Outcome::Text(t) => return t,
        Outcome::NewVersion(v) => {
            drop(o);
            let made = version_new(script, &v.language, Some(&v.original_language), None, &mut notes).unwrap();
            return s(&made.version);
        }
        Outcome::Append(a) => a,
    };
    let r = match &append.target {
        Target::Review { index, .. } => *index,
        Target::File(stored) => find_review(&o.session, stored).expect("the review is attached"),
        Target::Own => panic!("no review"),
    };
    let rows = o.row_resolver();
    let caps = |k: &ElementKind| o.fdx.is_caps(k.name());
    let applied = apply(&mut o.session, &o.doc, r, append, &caps, rows.as_deref()).unwrap();
    drop(rows);
    if let Some(text) = &applied.file_text {
        let path = o.review_path(r);
        o.write_review(&path, text).unwrap();
        o.save().unwrap();
    }
    applied.message
}

/// The CLI's `decide` (`decide_in`): under the lock, open, check, decide,
/// save. `entry` is the suggestion's entry in the review file.
fn decide(script: &Path, entry: usize, d: Decision) {
    let _lock = SessionLock::for_script(script).unwrap();
    let mut o = Opened::open(&s(script), &[], &mut vec![]).unwrap();
    let i = o.session.reviews[0].items.iter().position(|it| it.entry == entry).unwrap();
    o.session.check(&o.doc, 0, i, &d).unwrap_or_else(|e| panic!("e{entry}: {e:?}"));
    let sel = o.sel();
    o.session.decide(&mut o.doc, sel, 0, i, d).unwrap();
    o.save().unwrap();
}

/// The app's Write .fdx on the version: save the session, write in place.
fn write(script: &Path) -> String {
    let mut o = Opened::open(&s(script), &[], &mut vec![]).unwrap();
    o.save().unwrap();
    write_in_place(script, &o.raw).unwrap_or_else(|e| panic!("{e}")).message
}

/// Each row as `read_rows` would see it: status, needs, note, the version's
/// texts, and the entries placed on it with their status.
#[derive(Debug, PartialEq)]
struct Row {
    status: RowStatus,
    needs: bool,
    note: Option<Note>,
    version: Vec<String>,
    placed: Vec<(usize, Status)>,
}

fn rows(o: &Opened) -> Vec<Row> {
    let laid = o.lay_out_pair().expect("the pair loads");
    let v = o.pair().loaded().unwrap().links.sides().1;
    laid.rows
        .iter()
        .map(|r| Row {
            status: r.status,
            needs: r.needs,
            note: r.note,
            version: r.cells[v].iter().map(|id| o.text(*id)).collect(),
            placed: r
                .suggestions
                .iter()
                .map(|&n| {
                    let p = &laid.placed[n];
                    (o.session.reviews[p.review].items[p.item].entry, o.session.status(&o.doc, p.review, p.item))
                })
                .collect(),
        })
        .collect()
}

/// The entries of review 0 not placed on any line.
fn unmatched(o: &Opened) -> Vec<usize> {
    o.session.reviews[0].unmatched.iter().map(|u| u.entry).collect()
}

/// `read_rows` through the tool, `needs` only: the rows' numbers.
fn needs(script: &Path) -> Vec<u64> {
    let v: Value = serde_json::from_str(&tool(script, "read_rows", json!({ "needs": true }), None)).unwrap();
    v["rows"].as_array().unwrap().iter().map(|r| r["row"].as_u64().unwrap()).collect()
}

/// The `this` (version) ids of `read_rows`' row `n`.
fn row_ids(script: &Path, n: u64) -> Vec<u64> {
    let v: Value =
        serde_json::from_str(&tool(script, "read_rows", json!({ "from_row": n, "limit": 1 }), None)).unwrap();
    v["rows"][0]["this"].as_array().unwrap().iter().map(|p| p["id"].as_u64().unwrap()).collect()
}

/// Replace `from` (exactly once) in `text`.
fn once(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "{from:?} in\n{text}");
    text.replacen(from, to, 1)
}

fn para(kind: &str, text: &str) -> String {
    format!("    <Paragraph Type=\"{kind}\">\n      <Text>{text}</Text>\n    </Paragraph>\n")
}

#[test]
fn a_version_from_new_to_written_twice_through_final_draft_edits() {
    use RowStatus::*;
    let f = Folder::new("fixture");
    f.write("film.fdx", SCRIPT);
    let en = f.at("film.fdx");

    // new_version, on the original.
    let es = PathBuf::from(tool(&en, "new_version", json!({ "language": "es", "original_language": "en" }), None));
    assert_eq!(es, f.at("film.es.fdx"));
    let header = "SCRIPT REVIEW v1\nScript: film.es.fdx\n\n";
    f.write(REVIEW, header);
    let mut o = Opened::open(&s(&es), &[&s(&f.at(REVIEW))], &mut vec![]).unwrap();
    o.save().unwrap();
    drop(o);
    // Every row but the cues needs work.
    assert_eq!(needs(&es), [1, 2, 4, 5, 7, 8, 9, 10, 12, 13]);

    // One batch: the heading, a two-option line, a parenthetical, a block of
    // two lines for one blank action.
    let id = |n| row_ids(&es, n)[0];
    let batch = json!([
        { "row": 1, "paragraph_id": id(1), "options": ["EXT. CASA DE CAMPO - NOCHE"] },
        { "row": 2, "paragraph_id": id(2), "options": ["Laura sonríe."] },
        { "row": 4, "paragraph_id": id(4), "options": ["Mañana voy al pueblo.", "Mañana me voy al pueblo."],
          "why": "2 is softer" },
        { "row": 5, "paragraph_id": id(5), "options": ["Laura ríe."] },
        { "row": 7, "paragraph_id": id(7), "options": ["(en voz baja)"] },
        { "row": 8, "paragraph_id": id(8), "options": ["Llévame contigo."] },
        { "row": 9, "paragraph_id": id(9), "options": ["INT. GRANERO - DÍA"] },
        { "row": 10, "paragraph_ids": [id(10)], "options": [[
            { "kind": "Action", "text": "Hay paja por todas partes." },
            { "kind": "Action", "text": "Las vacas miran." },
        ]] },
        { "row": 12, "paragraph_id": id(12), "options": ["Salimos al amanecer."] },
    ]);
    let out: Value =
        serde_json::from_str(&tool(&es, "suggest_changes", json!({ "suggestions": batch }), Some(REVIEW))).unwrap();
    assert_eq!(out["added"], 9, "{out}");
    assert_eq!(f.read(REVIEW).matches("\nROW: ").count(), 9);
    assert_eq!(needs(&es), [13], "every suggested row is taken");

    // Decide, as the CLI does: entries are the batch's order.
    decide(&es, 0, Decision::Accept(0));
    decide(&es, 1, Decision::Accept(0));
    decide(&es, 2, Decision::Accept(1));
    decide(&es, 3, Decision::KeepMine);
    decide(&es, 4, Decision::Rewrite("(bajito)".into()));
    // e5 stays pending.
    decide(&es, 6, Decision::Accept(0));
    decide(&es, 7, Decision::Accept(0));
    decide(&es, 8, Decision::Accept(0));

    let o = Opened::open(&s(&es), &[], &mut vec![]).unwrap();
    assert_eq!(unmatched(&o), Vec::<usize>::new());
    let before: Vec<RowStatus> = rows(&o).iter().map(|r| r.status).collect();
    assert_eq!(
        before,
        [InSync, InSync, InSync, InSync, Blank, InSync, InSync, Blank, InSync, InSync, InSync, InSync, Blank]
    );
    drop(o);

    // Write .fdx: the version written in place.
    assert_eq!(write(&es), "Wrote film.es.fdx. Undo starts again from here.");
    let written = f.read("film.es.fdx");
    for t in ["EXT. CASA DE CAMPO - NOCHE", "Mañana me voy al pueblo.", "(bajito)", "Las vacas miran."] {
        assert!(written.contains(t), "{t} in\n{written}");
    }
    assert!(!written.contains("Llévame contigo."), "pending is not applied");
    let o = Opened::open(&s(&es), &[], &mut vec![]).unwrap();
    // Every suggestion placed, the accepted block (e7, one blank line made
    // two) too, decided only in its file now.
    assert_eq!(unmatched(&o), Vec::<usize>::new());
    assert_eq!(rows(&o).iter().map(|r| r.status).collect::<Vec<_>>(), before, "the same after the write");
    drop(o);

    // Final Draft, the version: the heading retyped, "Laura sonríe." split
    // in two, the last blank line translated, re-saved with other
    // whitespace.
    let mut v = f.read("film.es.fdx");
    v = once(&v, "<Text>EXT. CASA DE CAMPO - NOCHE</Text>", "<Text>EXT. CASA DE CAMPO - DE NOCHE</Text>");
    v = once(&v, &para("Action", "Laura sonríe."), &(para("Action", "Laura") + &para("Action", "sonríe.")));
    let last_blank = para("Action", "");
    let at = v.rfind(&last_blank).expect("the last blank action");
    v.replace_range(at..at + last_blank.len(), &para("Action", "Ella asiente."));
    v = v.replace("standalone=\"no\" ?>", "standalone=\"no\"?>").replace("\n    <Paragraph", "\n\t<Paragraph");
    f.write("film.es.fdx", &v);

    // Final Draft, the original: a line added, one reworded, other whitespace.
    let mut e = f.read("film.fdx");
    e = once(
        &e,
        &para("Action", "Laura laughs."),
        &(para("Action", "Laura laughs.") + &para("Action", "A dog barks.")),
    );
    e = once(&e, "<Text>We leave at dawn.</Text>", "<Text>We leave at first light.</Text>");
    e = e.replace("\n      <Text>", "\n\t\t<Text>");
    f.write("film.fdx", &e);

    // Reopened: both re-found, the old session set aside.
    // The session holds only the decisions the review files hold (the lines
    // Final Draft changed just aren't found now): set aside with a note.
    let mut notes = vec![];
    let mut o = Opened::open(&s(&es), &[], &mut notes).unwrap_or_else(|e| panic!("{e}"));
    for file in ["film.es.fdx", "film.fdx"] {
        assert!(notes.iter().any(|n| n.starts_with(&format!("{file} changed outside Scriptorium"))), "{notes:?}");
    }
    assert!(notes.iter().any(|n| n.contains("its session held only decisions")), "{notes:?}");
    o.save().unwrap();
    let got = rows(&o);
    let want = |status, needs, note, version: &[&str], placed: &[(usize, Status)]| Row {
        status,
        needs,
        note,
        version: version.iter().map(|t| t.to_string()).collect(),
        placed: placed.to_vec(),
    };
    let expected = vec![
        // The heading retyped: its decision reads none of its texts now.
        // Split: the joined halves read like the old line, so it pairs with
        // the first half, "Laura", and "sonríe.", unlisted, follows it into
        // the same row (D3).
        want(ThisChanged, false, None, &["EXT. CASA DE CAMPO - DE NOCHE"], &[]),
        want(ThisChanged, false, None, &["Laura", "sonríe."], &[]),
        want(InSync, false, None, &["CESAR"], &[]),
        want(InSync, false, None, &["Mañana me voy al pueblo."], &[(2, Status::Accepted(1))]),
        // Kept blank: placed and kept, still blank, needs nothing.
        want(Blank, false, None, &[""], &[(3, Status::Rejected)]),
        // New in the original: a row of its own.
        want(OneSided, false, Some(Note::NothingOnVersionSide), &[], &[]),
        want(InSync, false, None, &["LAURA"], &[]),
        want(InSync, false, None, &["(bajito)"], &[(4, Status::Rewritten)]),
        want(Blank, false, None, &[""], &[(5, Status::Pending)]),
        want(InSync, false, None, &["INT. GRANERO - DÍA"], &[(6, Status::Accepted(0))]),
        want(InSync, false, None, &["Hay paja por todas partes.", "Las vacas miran."], &[(7, Status::Accepted(0))]),
        want(InSync, false, None, &["CESAR"], &[]),
        // Reworded in English: its suggestion isn't found, the row needs an update.
        want(OtherChanged, true, None, &["Salimos al amanecer."], &[]),
        // Translated in Final Draft.
        want(ThisChanged, false, None, &["Ella asiente."], &[]),
    ];
    for (i, (g, w)) in got.iter().zip(&expected).enumerate() {
        assert_eq!(g, w, "row {}", i + 1);
    }
    assert_eq!(got.len(), expected.len());
    assert_eq!(unmatched(&o), [0, 1, 8]);
    drop(o);

    // The agent: only the reworded row needs work; the update goes into the
    // same review.
    assert_eq!(needs(&es), [13]);
    let update =
        json!([{ "row": 13, "paragraph_id": row_ids(&es, 13)[0], "options": ["Salimos con la primera luz."] }]);
    let out: Value =
        serde_json::from_str(&tool(&es, "suggest_changes", json!({ "suggestions": update }), Some(REVIEW))).unwrap();
    assert_eq!(out["added"], 1, "{out}");
    assert_eq!(needs(&es), Vec::<u64>::new());
    decide(&es, 9, Decision::Accept(0));

    // The second write: the reworded row in sync, its stamps the new texts.
    assert_eq!(write(&es), "Wrote film.es.fdx. Undo starts again from here.");
    let o = Opened::open(&s(&es), &[], &mut vec![]).unwrap();
    let got = rows(&o);
    assert_eq!(got[12].status, InSync);
    assert_eq!(got[12].placed, [(9, Status::Accepted(0))]);
    assert_eq!(got[12].version, ["Salimos con la primera luz."]);
    let statuses: Vec<RowStatus> = got.iter().map(|r| r.status).collect();
    assert_eq!(
        statuses,
        [
            ThisChanged,
            ThisChanged,
            InSync,
            InSync,
            Blank,
            OneSided,
            InSync,
            InSync,
            Blank,
            InSync,
            InSync,
            InSync,
            InSync,
            ThisChanged
        ]
    );
    assert_eq!(unmatched(&o), [0, 1, 8]);
    let l = links::parse(&f.read("film.es.links.txt")).unwrap();
    let stamps: Vec<Option<Stamp>> = l.rows[12].cells[1].iter().map(|id| l.paras[1][id].stamp.clone()).collect();
    assert_eq!(stamps, [Some(Stamp::Synced("Salimos con la primera luz.".into()))]);
    drop(o);
    assert_eq!(needs(&es), Vec::<u64>::new());
}

/// A folder with `film.fdx` (the fixture), its version made through the
/// tools, and the empty review attached to it; the version's path.
fn made(name: &str) -> (Folder, PathBuf) {
    let f = Folder::new(name);
    f.write("film.fdx", SCRIPT);
    let es = PathBuf::from(tool(
        &f.at("film.fdx"),
        "new_version",
        json!({ "language": "es", "original_language": "en" }),
        None,
    ));
    f.write(REVIEW, "SCRIPT REVIEW v1\nScript: film.es.fdx\n\n");
    let mut o = Opened::open(&s(&es), &[&s(&f.at(REVIEW))], &mut vec![]).unwrap();
    o.save().unwrap();
    (f, es)
}

/// A block suggestion on a blank row whose option has another number of
/// lines (one blank action made two), accepted and written in place, is
/// found on the next open, accepted (found by A12: `on_row` used to compare
/// the row's cell with `BEFORE`'s length for a block decided only in its
/// file).
#[test]
fn a_decided_block_of_another_length_is_placed_after_a_write() {
    let (_f, es) = made("block");
    let block = json!([{ "row": 10, "paragraph_ids": [row_ids(&es, 10)[0]], "options": [[
        { "kind": "Action", "text": "Hay paja por todas partes." },
        { "kind": "Action", "text": "Las vacas miran." },
    ]] }]);
    tool(&es, "suggest_changes", json!({ "suggestions": block }), Some(REVIEW));
    decide(&es, 0, Decision::Accept(0));
    write(&es);
    let o = Opened::open(&s(&es), &[], &mut vec![]).unwrap();
    assert_eq!(unmatched(&o), Vec::<usize>::new(), "{:?}", o.session.reviews[0].unmatched);
    assert_eq!(rows(&o)[9].placed, [(0, Status::Accepted(0))]);
}

/// BUG (A12): after a decision on a row (saved, its progress in the review
/// file), Final Draft retypes that line of the version. The CLI's open then
/// refuses ("its session holds decisions that aren't saved in a review
/// file") though the review file holds the decision: `aside::only_decisions`
/// compares the session's decisions with those the fresh open *placed*, and
/// an entry whose line was retyped isn't placed. The plan ("Opening a
/// version from the CLI after a Final Draft save") compares with the review
/// files' saved progress.
#[test]
fn a_final_draft_edit_to_a_decided_line_still_opens_from_the_cli() {
    let (f, es) = made("retyped");
    let line = json!([{ "row": 2, "paragraph_id": row_ids(&es, 2)[0], "options": ["Laura sonríe."] }]);
    tool(&es, "suggest_changes", json!({ "suggestions": line }), Some(REVIEW));
    decide(&es, 0, Decision::Accept(0));
    // Written from the app, then retyped in Final Draft.
    write(&es);
    let v = once(&f.read("film.es.fdx"), "<Text>Laura sonríe.</Text>", "<Text>Laura sonríe apenas.</Text>");
    f.write("film.es.fdx", &v);
    let mut notes = vec![];
    let o = Opened::open(&s(&es), &[], &mut notes).unwrap_or_else(|e| panic!("{e}"));
    assert!(notes.iter().any(|n| n.contains("its session held only decisions")), "{notes:?}");
    assert_eq!(rows(&o)[1].status, RowStatus::ThisChanged);
}

/// `read_rows`' rows, as JSON, on `script` with `args`.
fn read_rows(script: &Path, args: Value) -> Vec<Value> {
    let v: Value = serde_json::from_str(&tool(script, "read_rows", args, None)).unwrap();
    v["rows"].as_array().unwrap().clone()
}

#[test]
fn the_real_script_from_new_to_written_twice() {
    let real = std::env::var("SCRIPTORIUM_REAL_FDX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../2026_11_05_PURPLE LIST.fdx"));
    let Ok(raw) = std::fs::read_to_string(&real) else {
        eprintln!("skipped: {} not found", real.display());
        return;
    };
    // A copy: nothing is ever written beside the real script.
    let f = Folder::new("real");
    f.write("PURPLE LIST.fdx", &raw);
    let en = f.at("PURPLE LIST.fdx");
    let es = PathBuf::from(tool(&en, "new_version", json!({ "language": "es", "original_language": "en" }), None));
    f.write(REVIEW, "SCRIPT REVIEW v1\nScript: PURPLE LIST.es.fdx\n\n");
    let mut o = Opened::open(&s(&es), &[&s(&f.at(REVIEW))], &mut vec![]).unwrap();
    o.save().unwrap();
    let made_rows = rows(&o);
    let (count, total) = (made_rows.len(), made_rows.iter().filter(|r| r.needs).count());
    drop(o);
    let all = read_rows(&es, json!({ "limit": 200 }));

    // From the first rows that need work: the first heading, and four
    // one-line rows whose English is plain text found once in the file.
    let one = |r: &Value, side: &str| r[side].as_array().unwrap().len() == 1;
    let text = |r: &Value| r["other"][0]["text"].as_str().unwrap().to_string();
    let plain = |t: &str| {
        t.len() > 12
            && t.chars().all(|c| c.is_ascii_alphanumeric() || " .,!?".contains(c))
            && raw.matches(&format!("<Text>{t}</Text>")).count() == 1
    };
    let needing: Vec<&Value> = all.iter().filter(|r| r["needs"] == true && one(r, "this") && one(r, "other")).collect();
    let heading = *needing.iter().find(|r| r["other"][0]["kind"] == "Scene Heading").expect("a heading");
    let lines: Vec<&Value> = needing
        .iter()
        .copied()
        .filter(|r| ["Action", "Dialogue"].contains(&r["other"][0]["kind"].as_str().unwrap()) && plain(&text(r)))
        .take(4)
        .collect();
    assert_eq!(lines.len(), 4, "four plain lines in the first 200 rows");
    let n = |r: &Value| r["row"].as_u64().unwrap();
    let id = |r: &Value| r["this"][0]["id"].as_u64().unwrap();
    let batch = json!([
        { "row": n(heading), "paragraph_id": id(heading), "options": ["ESCENA UNO"] },
        { "row": n(lines[0]), "paragraph_id": id(lines[0]), "options": ["Línea uno.", "Línea uno, otra."] },
        { "row": n(lines[1]), "paragraph_id": id(lines[1]), "options": ["Línea dos."] },
        { "row": n(lines[2]), "paragraph_id": id(lines[2]), "options": ["Línea tres."] },
        { "row": n(lines[3]), "paragraph_id": id(lines[3]), "options": ["Línea cuatro."] },
    ]);
    let out: Value =
        serde_json::from_str(&tool(&es, "suggest_changes", json!({ "suggestions": batch }), Some(REVIEW))).unwrap();
    assert_eq!(out["added"], 5, "{out}");
    decide(&es, 0, Decision::Accept(0));
    decide(&es, 1, Decision::Accept(1));
    decide(&es, 2, Decision::KeepMine);
    decide(&es, 3, Decision::Rewrite("Línea tres, mía.".into()));
    // e4 stays pending.
    assert_eq!(write(&es), "Wrote PURPLE LIST.es.fdx. Undo starts again from here.");
    assert_eq!(f.read("PURPLE LIST.fdx"), raw, "the original is never written");

    // Final Draft: the English line of the accepted row reworded, a line
    // added after the rewritten row's; both files re-saved with other
    // whitespace.
    let reworded = text(lines[0]);
    let mut e = once(&raw, &format!("<Text>{reworded}</Text>"), "<Text>Something else entirely.</Text>");
    let added_after = format!("<Text>{}</Text>", text(lines[2]));
    let at = e.find(&added_after).unwrap();
    let end = at + e[at..].find("</Paragraph>").unwrap() + "</Paragraph>".len();
    e.insert_str(end, "\n    <Paragraph Type=\"Action\">\n      <Text>A dog barks.</Text>\n    </Paragraph>");
    f.write("PURPLE LIST.fdx", &e.replace("\n    <Paragraph", "\n  <Paragraph"));
    let v = f.read("PURPLE LIST.es.fdx");
    f.write("PURPLE LIST.es.fdx", &v.replace("\n    <Paragraph", "\n\t<Paragraph"));

    // The session holds only what the review file holds (the reworded
    // row's decision just isn't placed now): set aside with a note.
    let mut notes = vec![];
    let mut o = Opened::open(&s(&es), &[], &mut notes).unwrap_or_else(|e| panic!("{e}"));
    for file in ["PURPLE LIST.es.fdx", "PURPLE LIST.fdx"] {
        assert!(notes.iter().any(|n| n.starts_with(&format!("{file} changed outside Scriptorium"))), "{notes:?}");
    }
    assert!(notes.iter().any(|n| n.contains("its session held only decisions")), "{notes:?}");
    o.save().unwrap();
    assert_eq!(unmatched(&o), [1], "only the reworded row's suggestion is lost");
    let got = rows(&o);
    let row = |r: &Value| {
        let k = n(r) as usize - 1;
        // Rows after the added line move down one.
        if k > n(lines[2]) as usize - 1 { k + 1 } else { k }
    };
    assert_eq!(got[row(heading)].status, RowStatus::InSync);
    assert_eq!(got[row(heading)].placed, [(0, Status::Accepted(0))]);
    assert_eq!(got[row(heading)].version, ["ESCENA UNO"]);
    assert_eq!((got[row(lines[0])].status, got[row(lines[0])].needs), (RowStatus::OtherChanged, true));
    assert_eq!(got[row(lines[0])].version, ["Línea uno, otra."]);
    assert_eq!(got[row(lines[1])].status, RowStatus::Blank);
    assert_eq!((got[row(lines[1])].placed.clone(), got[row(lines[1])].needs), (vec![(2, Status::Rejected)], false));
    assert_eq!(got[row(lines[2])].status, RowStatus::InSync);
    assert_eq!(got[row(lines[2])].placed, [(3, Status::Rewritten)]);
    let dog = row(lines[2]) + 1;
    assert_eq!((got[dog].status, got[dog].note), (RowStatus::OneSided, Some(Note::NothingOnVersionSide)));
    assert_eq!(got[row(lines[3])].status, RowStatus::Blank);
    assert_eq!((got[row(lines[3])].placed.clone(), got[row(lines[3])].needs), (vec![(4, Status::Pending)], false));
    assert_eq!(got.len(), count + 1);
    let needs_now = got.iter().filter(|r| r.needs).count();
    assert_eq!(needs_now, total - 5 + 1, "the suggested rows done, the reworded one back");
    drop(o);

    // The update, in the same review, accepted; the second write.
    let k = row(lines[0]) as u64 + 1;
    let upd = json!([{ "row": k, "paragraph_id": row_ids(&es, k)[0], "options": ["Otra cosa del todo."] }]);
    let out: Value =
        serde_json::from_str(&tool(&es, "suggest_changes", json!({ "suggestions": upd }), Some(REVIEW))).unwrap();
    assert_eq!(out["added"], 1, "{out}");
    decide(&es, 5, Decision::Accept(0));
    write(&es);
    let o = Opened::open(&s(&es), &[], &mut vec![]).unwrap();
    let got = rows(&o);
    assert_eq!(got[row(lines[0])].status, RowStatus::InSync);
    assert_eq!(got[row(lines[0])].placed, [(5, Status::Accepted(0))]);
    assert_eq!(got.iter().filter(|r| r.needs).count(), total - 5);
}
