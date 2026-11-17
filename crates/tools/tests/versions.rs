//! `list_versions` and `read_rows` (plan step A8) over a pair laid out in
//! memory: a placeholder version of a small script (`blank_copy`), its rows
//! laid out with `scriptorium_versions::lay_out` as the hosts do. Also
//! `new_version` and suggestions on rows (plan step A9).

use std::borrow::Cow;
use std::path::Path;

use scriptorium_core::{Document, ElementKind, ParaId, Paragraph, Script, Selection};
use scriptorium_fdx::FdxFile;
use scriptorium_review::{Decision, Line, Review, RowRef, RowResolver, Session, Status};
use scriptorium_tools::{
    Append, Applied, Ctx, LinksCtx, NewVersion, Outcome, ROWS_BYTES, ToolError, already_made, apply, call, may_append,
    new_version_message, uses_links, writes,
};
use scriptorium_versions::{Links, Member, PairView, Placed, PlacedState, Resolver, RowQuery, blank_copy, lay_out};
use serde_json::{Value, json};

const SCRIPT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>EXT. FARM HOUSE - NIGHT</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am going to town tomorrow.</Text></Paragraph>
    <Paragraph Type="Scene Heading"><Text>INT. BARN - DAY</Text></Paragraph>
    <Paragraph Type="Action"><Text>Hay everywhere.</Text></Paragraph>
  </Content>
</FinalDraft>
"#;

const LINKS: &str = "/films/film.es.links.txt";

struct Pair {
    fdx: FdxFile,
    original: Script,
    version: Document,
    /// The version's file as made (blank lines).
    blank: Script,
    links: Links,
    session: Session,
}

impl Pair {
    fn new() -> Self {
        Self::of(SCRIPT)
    }

    fn of(raw: &str) -> Self {
        let fdx = FdxFile::parse(raw.to_string()).unwrap();
        let original = fdx.to_script();
        let en = Member { file: "film.fdx".into(), language: Some("en".into()), hash: "h0".into(), created: None };
        let es = Member { file: "film.es.fdx".into(), language: Some("es".into()), hash: "h1".into(), created: None };
        let (version, links) =
            blank_copy(&original, "lk-test", en, es, |k: &ElementKind| fdx.is_caps(k.name())).unwrap();
        Pair {
            fdx,
            original,
            blank: version.clone(),
            version: Document::new(version),
            links,
            session: Session::default(),
        }
    }

    /// The rows laid out on member `this`, with `placed` on the version.
    fn links_ctx(&self, this: usize, placed: &[Placed]) -> LinksCtx<'_> {
        let is_caps = |k: &ElementKind| self.fdx.is_caps(k.name());
        let rows = lay_out(&self.links, &self.original, self.version.script(), placed, &is_caps);
        let scripts = vec![Cow::Borrowed(&self.original), Cow::Borrowed(self.version.script())];
        LinksCtx::from(PairView { links: &self.links, path: Path::new(LINKS), this, rows, scripts, placed: vec![] })
    }

    fn call(&self, links: Option<Result<LinksCtx<'_>, String>>, name: &str, args: Value) -> Result<String, ToolError> {
        match self.outcome(links, name, args)? {
            Outcome::Text(t) => Ok(t),
            o => panic!("{name} has effects: {o:?}"),
        }
    }

    fn outcome(
        &self,
        links: Option<Result<LinksCtx<'_>, String>>,
        name: &str,
        args: Value,
    ) -> Result<Outcome, ToolError> {
        let is_caps = |k: &ElementKind| self.fdx.is_caps(k.name());
        let script_name = if links.as_ref().and_then(|l| l.as_ref().ok()).is_some_and(|l| l.this == 0) {
            "film.fdx"
        } else {
            "film.es.fdx"
        };
        let ctx = Ctx {
            doc: &self.version,
            session: &self.session,
            script_name,
            is_caps: &is_caps,
            default_review: None,
            links,
        };
        call(&ctx, name, &args)
    }

    /// `read_rows`' whole result: `{rows, next_from_row}`.
    fn page(&self, this: usize, placed: &[Placed], args: Value) -> Value {
        serde_json::from_str(&self.call(Some(Ok(self.links_ctx(this, placed))), "read_rows", args).unwrap()).unwrap()
    }

    fn rows(&self, this: usize, placed: &[Placed], args: Value) -> Value {
        self.page(this, placed, args)["rows"].clone()
    }
}

fn needs(rows: &Value) -> Vec<(u64, bool)> {
    rows.as_array().unwrap().iter().map(|r| (r["row"].as_u64().unwrap(), r["needs"].as_bool().unwrap())).collect()
}

#[test]
fn on_a_new_version_every_blank_row_needs_work() {
    let p = Pair::new();
    let rows = p.rows(1, &[], json!({}));
    assert_eq!(needs(&rows), [(1, true), (2, true), (3, false), (4, true), (5, true), (6, true)]);
    assert_eq!(rows[1]["status"], "blank");
    assert_eq!(rows[1]["label"], "not yet translated");
    assert_eq!(rows[1]["this"], json!([{ "id": 1, "kind": "Action", "text": "" }]), "blank lines are shown");
    assert_eq!(rows[1]["other"][0]["text"], "Laura smiles.");
    assert_eq!(rows[1]["suggestions"], json!([]));
    assert_eq!(rows[2]["status"], "in_sync");
    // One row per line.
    let text = p.call(Some(Ok(p.links_ctx(1, &[]))), "read_rows", json!({})).unwrap();
    assert_eq!(text.lines().count(), 6 + 3, "{text}");
    assert!(text.ends_with("\"next_from_row\": null}"), "{text}");
}

#[test]
fn a_pending_or_kept_row_needs_nothing() {
    let p = Pair::new();
    for state in [PlacedState::Pending, PlacedState::Kept] {
        let placed = Placed { cell: vec![ParaId(1)], state, row_text: vec!["Laura smiles.".into()] };
        let rows = p.rows(1, &[placed], json!({ "needs": true }));
        let listed: Vec<u64> = needs(&rows).into_iter().map(|(n, _)| n).collect();
        assert_eq!(listed, [1, 4, 5, 6], "{state:?}");
    }
}

#[test]
fn on_the_original_nothing_needs_work_and_this_is_the_original() {
    let p = Pair::new();
    let rows = p.rows(0, &[], json!({}));
    assert!(needs(&rows).iter().all(|(_, n)| !n));
    assert_eq!(rows[1]["this"][0]["text"], "Laura smiles.");
    assert_eq!(rows[1]["other"][0]["text"], "");
    let e = p.call(Some(Ok(p.links_ctx(0, &[]))), "read_rows", json!({ "needs": true })).unwrap_err();
    assert!(e.to_string().contains("it is the original"), "{e}");
    assert!(e.to_string().contains("/films/film.es.fdx"), "{e}");
}

#[test]
fn rows_by_scene_and_page() {
    let p = Pair::new();
    let rows = p.rows(1, &[], json!({ "scene": 2 }));
    assert_eq!(needs(&rows).iter().map(|r| r.0).collect::<Vec<_>>(), [5, 6]);
    // Scene by heading, on this side: the version's headings are copied.
    let rows = p.rows(1, &[], json!({ "scene": "barn" }));
    assert_eq!(rows.as_array().unwrap().len(), 2);
    let page = p.page(1, &[], json!({ "from_row": 2, "limit": 2 }));
    assert_eq!(needs(&page["rows"]).iter().map(|r| r.0).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(page["next_from_row"], 4);
    // With needs, the next page starts at the next row that needs work.
    let page = p.page(1, &[], json!({ "needs": true, "limit": 2 }));
    assert_eq!(page["next_from_row"], 4);
    assert_eq!(p.page(1, &[], json!({ "from_row": 99 })), json!({ "rows": [], "next_from_row": null }));
    let bad = p.call(Some(Ok(p.links_ctx(1, &[]))), "read_rows", json!({ "from_row": 0 }));
    assert!(bad.unwrap_err().to_string().contains("from_row"));
    let bad = p.call(Some(Ok(p.links_ctx(1, &[]))), "read_rows", json!({ "scene": 9 }));
    assert!(bad.is_err());
}

#[test]
fn list_versions_names_the_version_with_absolute_paths() {
    let p = Pair::new();
    let v: Value =
        serde_json::from_str(&p.call(Some(Ok(p.links_ctx(0, &[]))), "list_versions", json!({})).unwrap()).unwrap();
    let pair = &v["pair"];
    assert_eq!(pair["links"], LINKS);
    assert_eq!(pair["id"], "lk-test");
    assert_eq!(pair["version"], "/films/film.es.fdx");
    assert_eq!(
        pair["this"],
        json!({
            "file": "film.fdx", "path": "/films/film.fdx", "language": "en", "label": "English",
            "made": false, "is_version": false,
        })
    );
    assert_eq!(pair["other"]["path"], "/films/film.es.fdx");
    assert_eq!(pair["other"]["made"], true);
    assert_eq!(pair["other"]["is_version"], true);
    assert_eq!(pair["needs"], 0, "nothing needs work on the original");
    assert_eq!(pair["rows"], 6);

    let v: Value =
        serde_json::from_str(&p.call(Some(Ok(p.links_ctx(1, &[]))), "list_versions", json!({})).unwrap()).unwrap();
    assert_eq!(v["pair"]["this"]["is_version"], true);
    assert_eq!(v["pair"]["needs"], 5);
    assert_eq!(v["pair"]["status"], json!({ "blank": 5, "in_sync": 1 }));
    assert_eq!(v["pair"]["new_in_original"], json!([]));
}

#[test]
fn no_pair_or_an_unusable_one() {
    let p = Pair::new();
    assert_eq!(
        serde_json::from_str::<Value>(&p.call(None, "list_versions", json!({})).unwrap()).unwrap(),
        json!({ "pair": null })
    );
    let e = p.call(None, "read_rows", json!({})).unwrap_err().to_string();
    assert!(e.starts_with("no version is lined up with film.es.fdx"), "{e}");
    for tool in ["list_versions", "read_rows"] {
        let e = p.call(Some(Err("film.es.links.txt, line 3: bad row".into())), tool, json!({})).unwrap_err();
        assert_eq!(e.to_string(), "film.es.links.txt, line 3: bad row");
    }
}

#[test]
fn a_heading_translated_on_the_version_is_found_by_the_originals_words() {
    let mut p = Pair::new();
    let mut es = p.version.script().clone();
    let at = es.paragraphs.iter().position(|q| q.text() == "INT. BARN - DAY").unwrap();
    es.paragraphs[at] = Paragraph::plain(es.paragraphs[at].id, ElementKind::SceneHeading, "INT. GRANERO - DÍA");
    p.version = Document::new(es);
    let rows = |q: &str| needs(&p.rows(1, &[], json!({ "scene": q }))).iter().map(|r| r.0).collect::<Vec<_>>();
    assert_eq!(rows("granero"), [5, 6]);
    assert_eq!(rows("barn"), [5, 6], "not on this side: the other file's headings");
    let e = p.call(Some(Ok(p.links_ctx(1, &[]))), "read_rows", json!({ "scene": "kitchen" })).unwrap_err();
    assert!(e.to_string().contains("kitchen"), "{e}");
}

#[test]
fn a_long_page_stops_early_and_says_where_to_go_on() {
    // Rows 2 and 3 are each 60% of a page, row 4 more than a whole page.
    let action =
        |bytes: usize| format!("<Paragraph Type=\"Action\"><Text>{}</Text></Paragraph>", "word ".repeat(bytes / 5));
    let actions = [action(ROWS_BYTES * 6 / 10), action(ROWS_BYTES * 6 / 10), action(ROWS_BYTES * 12 / 10)].concat();
    let raw = SCRIPT.replace("<Paragraph Type=\"Action\"><Text>Laura smiles.</Text></Paragraph>", &actions);
    let p = Pair::of(&raw);
    let rows_of = |page: &Value| page["rows"].as_array().unwrap().iter().map(|r| r["row"].clone()).collect::<Vec<_>>();
    let page = p.page(0, &[], json!({}));
    assert_eq!(rows_of(&page), [1, 2]);
    assert_eq!(page["next_from_row"], 3);
    let page = p.page(0, &[], json!({ "from_row": 3 }));
    assert_eq!(rows_of(&page), [3]);
    assert_eq!(page["next_from_row"], 4);
    let page = p.page(0, &[], json!({ "from_row": 4 }));
    assert_eq!(rows_of(&page), [4], "one row is always given, however long");
    assert_eq!(page["next_from_row"], 5);
}

#[test]
fn on_the_original_a_row_with_nothing_on_the_version_side_names_it() {
    let mut p = Pair::new();
    // The links file lists nothing on the version's side of row 2 (a line
    // new in the original).
    p.links.rows[1].cells[1].clear();
    let mut es = p.version.script().clone();
    es.paragraphs.retain(|q| q.id != ParaId(1));
    p.version = Document::new(es);
    let row = |this: usize| p.rows(this, &[], json!({}))[1].clone();
    let on_original = row(0);
    assert_eq!(on_original["status"], "one_sided");
    assert_eq!(on_original["label"], "nothing on the Español side");
    assert!(on_original["note"].as_str().unwrap().starts_with("nothing on the Español side"), "{on_original}");
    assert!(on_original["note"].as_str().unwrap().contains("film.es.fdx"), "{on_original}");
    let on_version = row(1);
    assert_eq!(on_version["label"], "nothing on this side");
    assert!(on_version["note"].as_str().unwrap().starts_with("nothing on this side"), "{on_version}");
}

// ---- Plan step A9: `new_version`, and suggestions on rows. ----

/// The resolver the hosts build for the version (`LoadedPair::resolver`).
fn resolver(p: &Pair) -> Box<RowResolver<'_>> {
    let r = Resolver::new(&p.links, &p.original, p.version.script(), &|k: &ElementKind| p.fdx.is_caps(k.name()));
    Box::new(move |row: &RowRef, texts: &[Vec<Line>], para: &[ParaId]| {
        let texts: Vec<Vec<String>> = texts.iter().map(|t| t.iter().map(|l| l.text.clone()).collect()).collect();
        let q = RowQuery { links_id: &row.id, row_text: &row.texts, texts: &texts, para };
        r.candidates(&q).map_err(|e| e.text())
    })
}

impl Pair {
    /// Attaches an empty review named `name`; returns its index.
    fn attach(&mut self, name: &str) -> usize {
        let is_caps = |k: &ElementKind| self.fdx.is_caps(k.name());
        let text = format!("SCRIPT REVIEW v1\nId: {}\n\n", name.len());
        let review =
            Review::open_live(name, &text, &self.original, &self.version, is_caps, "film.es.fdx", None).with_path(name);
        self.session.add(review).unwrap()
    }

    /// Opens review `r` again from its text, as reopening the script does
    /// (with the resolver for the pair as it is now).
    fn reopen(&mut self, r: usize) {
        let (name, text) = (self.session.reviews[r].name.clone(), self.session.reviews[r].text().to_string());
        let is_caps = |k: &ElementKind| self.fdx.is_caps(k.name());
        let rows = resolver(self);
        let review = Review::open_live(&name, &text, &self.blank, &self.version, is_caps, "film.es.fdx", Some(&*rows))
            .with_path(&name);
        drop(rows);
        self.session.reviews[r] = review;
    }

    /// `suggest_changes` on the version with `items` into `review_file`.
    fn suggest(&self, review_file: &str, items: Value) -> Result<Outcome, ToolError> {
        let args = json!({ "suggestions": items, "review_file": review_file });
        self.outcome(Some(Ok(self.links_ctx(1, &self.placed()))), "suggest_changes", args)
    }

    /// The `ROW:` suggestions of the session, as the hosts lay them out.
    fn placed(&self) -> Vec<Placed> {
        let mut out = vec![];
        for (r, review) in self.session.reviews.iter().enumerate() {
            for (i, it) in review.items.iter().enumerate() {
                let Some(row) = &it.row else { continue };
                let state = match self.session.status(&self.version, r, i) {
                    Status::Pending => PlacedState::Pending,
                    Status::Rejected => PlacedState::Kept,
                    Status::Accepted(_) => PlacedState::Accepted,
                    _ => PlacedState::Rewritten,
                };
                let cell = self.session.live_run(&self.version, r, i);
                let cell = if cell.is_empty() { vec![it.para] } else { cell };
                out.push(Placed { cell, state, row_text: row.texts.clone() });
            }
        }
        out
    }

    /// Adds `append` to review `r` as the hosts do (with the resolver).
    fn apply(&mut self, r: usize, append: Append) -> Applied {
        let mut session = std::mem::take(&mut self.session);
        let is_caps = |k: &ElementKind| self.fdx.is_caps(k.name());
        let rows = resolver(self);
        let applied = apply(&mut session, &self.version, r, append, &is_caps, Some(&*rows)).unwrap();
        drop(rows);
        self.session = session;
        applied
    }
}

fn append(o: Outcome) -> Append {
    match o {
        Outcome::Append(a) => a,
        o => panic!("not an append: {o:?}"),
    }
}

fn failed(o: Outcome) -> Vec<String> {
    let text = match o {
        Outcome::Text(t) => t,
        Outcome::Append(a) => return a.failed.into_iter().map(|f| f.error).collect(),
        o => panic!("{o:?}"),
    };
    let v: Value = serde_json::from_str(&text).unwrap();
    v["failed"].as_array().unwrap().iter().map(|f| f["error"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_batch_on_blank_rows_writes_empty_entries_with_their_rows() {
    let mut p = Pair::new();
    let r = p.attach("t.review.txt");
    let items = json!([
        { "row": 2, "paragraph_id": 1, "options": ["Laura sonríe."], "why": "" },
        { "row": 4, "paragraph_id": 3, "options": ["Mañana voy al pueblo.", "Mañana me voy al pueblo."], "why": "2 is softer" },
    ]);
    let a = append(p.suggest("t.review.txt", items).unwrap());
    assert!(a.failed.is_empty(), "{:?}", a.failed);
    assert_eq!(a.proposals[0].entry.row.as_ref().unwrap().texts, ["Laura smiles."]);
    let applied = p.apply(r, a);
    let v: Value = serde_json::from_str(&applied.message).unwrap();
    assert_eq!(v["added"], 2, "{v}");
    let text = applied.file_text.unwrap();
    assert!(
        text.contains("BEFORE: [empty]\nPARA: 1\nAFTER: Laura sonríe.\nWHY:\nROW: lk-test\nROW TEXT: Laura smiles.\n"),
        "{text}"
    );
    assert!(text.contains("ROW TEXT: I am going to town tomorrow.\n"), "{text}");
    assert_eq!(p.session.reviews[r].items.len(), 2);
    // Placed on its row: the row needs nothing now.
    let rows = p.rows(1, &p.placed(), json!({ "needs": true }));
    assert_eq!(needs(&rows).iter().map(|r| r.0).collect::<Vec<_>>(), [1, 5, 6]);
}

#[test]
fn a_block_on_a_blank_row_and_an_obvious_heading() {
    let mut p = Pair::new();
    let r = p.attach("t.review.txt");
    let items = json!([
        { "row": 2, "paragraph_ids": [1], "options": [[{ "kind": "Action", "text": "Laura sonríe." }]] },
        { "row": 1, "paragraph_id": 0, "options": ["EXT. GRANJA - NOCHE"] },
    ]);
    let a = append(p.suggest("t.review.txt", items).unwrap());
    assert!(a.failed.is_empty(), "{:?}", a.failed);
    let text = p.apply(r, a).file_text.unwrap();
    assert!(text.contains("BEFORE: EXT. FARM HOUSE - NIGHT\nPARA: 0\nAFTER: EXT. GRANJA - NOCHE\n"), "{text}");
    assert!(text.contains("BEFORE:\n  ACTION: \nPARA: 1\n"), "a typed blank line: {text}");
    assert_eq!(text.matches("ROW: lk-test\n").count(), 2, "{text}");
}

#[test]
fn a_target_not_in_the_row_is_refused_with_the_rows_ids() {
    let mut p = Pair::new();
    p.attach("t.review.txt");
    let errors = failed(p.suggest("t.review.txt", json!([{ "row": 2, "paragraph_id": 3, "options": ["x"] }])).unwrap());
    assert!(errors[0].contains("row 2 is ¶1 on this side") && errors[0].contains("paragraph_id 1"), "{errors:?}");
    let errors =
        failed(p.suggest("t.review.txt", json!([{ "row": 99, "paragraph_id": 3, "options": ["x"] }])).unwrap());
    assert!(errors[0].contains("no row 99"), "{errors:?}");
}

#[test]
fn rows_are_suggested_on_the_version_only() {
    let p = Pair::new();
    let args = json!({ "row": 2, "paragraph_id": 1, "options": ["Laura grins."] });
    let e = p.outcome(Some(Ok(p.links_ctx(0, &[]))), "suggest_change", args.clone()).unwrap_err().to_string();
    assert!(e.contains("rows are suggested on the version, /films/film.es.fdx"), "{e}");
    let e = p.outcome(None, "suggest_change", args.clone()).unwrap_err().to_string();
    assert!(e.contains("no version is lined up"), "{e}");
    let e = p.outcome(Some(Err("broken links".into())), "suggest_change", args).unwrap_err().to_string();
    assert!(e.starts_with("broken links. Suggestions on a version are placed by its links file"), "{e}");
    // Without `row` too: a version's suggestions go on rows.
    let plain = json!({ "paragraph_id": 2, "options": ["Cesar"] });
    let e = p.outcome(Some(Err("broken links".into())), "suggest_change", plain).unwrap_err().to_string();
    assert!(e.starts_with("broken links. "), "{e}");
}

#[test]
fn a_blank_target_outside_a_version_is_still_refused() {
    let p = Pair::new();
    let args = json!({ "paragraph_id": 1, "options": ["Laura sonríe."] });
    let e = p.outcome(None, "suggest_change", args).unwrap_err().to_string();
    assert!(e.contains("paragraph 1 is empty"), "{e}");
    let args = json!({ "paragraph_ids": [1], "options": [[{ "kind": "Action", "text": "Laura sonríe." }]] });
    let e = p.outcome(None, "suggest_change", args).unwrap_err().to_string();
    assert!(e.contains("every listed paragraph is empty"), "{e}");
}

#[test]
fn a_decided_row_has_nothing_to_update_until_the_original_changes() {
    let mut p = Pair::new();
    let name = "Translation from English.review.txt";
    let r = p.attach(name);
    let item = json!([{ "row": 2, "paragraph_id": 1, "options": ["Laura sonríe."] }]);
    let a = append(p.suggest(name, item).unwrap());
    p.apply(r, a);
    let sel = Selection::cursor(p.version.start());
    p.session.decide(&mut p.version, sel, r, 0, Decision::Accept(0)).unwrap();
    assert_eq!(p.version.paragraph(ParaId(1)).unwrap().text(), "Laura sonríe.");

    let update = json!([{ "paragraph_id": 1, "options": ["Laura sonríe apenas."] }]);
    let errors = failed(p.suggest(name, update.clone()).unwrap());
    assert_eq!(errors, [format!("row 2 is already decided in review {name} (#1, accepted 1); nothing to update")]);

    // The original's line changes: the decided suggestion isn't found any
    // more (its ROW TEXT is gone), and the update goes in the same review.
    let at = p.original.paragraphs.iter().position(|q| q.id == ParaId(1)).unwrap();
    p.original.paragraphs[at] = Paragraph::plain(ParaId(1), ElementKind::Action, "Laura laughs.");
    p.reopen(r);
    assert!(p.session.reviews[r].items.is_empty());
    let a = append(p.suggest(name, update).unwrap());
    assert_eq!(a.proposals[0].entry.row.as_ref().unwrap().texts, ["Laura laughs."]);
    let applied = p.apply(r, a);
    let v: Value = serde_json::from_str(&applied.message).unwrap();
    assert_eq!(v["added"], 1, "{v}");
    assert!(applied.file_text.unwrap().contains("BEFORE: Laura sonríe.\nPARA: 1\n"));
}

#[test]
fn a_pending_suggestion_not_found_right_now_is_not_doubled() {
    let mut p = Pair::new();
    let r = p.attach("t.review.txt");
    let a = append(p.suggest("t.review.txt", json!([{ "paragraph_id": 1, "options": ["Laura sonríe."] }])).unwrap());
    p.apply(r, a);
    // The writer types into the blank line by hand: the pending suggestion
    // reads none of its texts there.
    let mut es = p.version.script().clone();
    let at = es.paragraphs.iter().position(|q| q.id == ParaId(1)).unwrap();
    es.paragraphs[at] = Paragraph::plain(ParaId(1), ElementKind::Action, "Laura ríe.");
    p.version = Document::new(es);
    p.reopen(r);
    assert!(p.session.reviews[r].items.is_empty());
    let errors =
        failed(p.suggest("t.review.txt", json!([{ "paragraph_id": 1, "options": ["Laura sonríe."] }])).unwrap());
    assert!(
        errors[0].starts_with("row 2 already has a suggestion in review t.review.txt (e0) not found right now: "),
        "{errors:?}"
    );
}

#[test]
fn the_row_is_found_from_the_lines() {
    let p = Pair::new();
    // No `row`: the line's row is used.
    let a = append(
        p.suggest("t.review.txt", json!([{ "paragraph_id": 3, "options": ["Mañana voy al pueblo."] }])).unwrap(),
    );
    assert_eq!(a.proposals[0].entry.row.as_ref().unwrap().texts, ["I am going to town tomorrow."]);
    // A row number written as a string.
    let a =
        append(p.suggest("t.review.txt", json!([{ "row": "4", "paragraph_id": 3, "options": ["Mañana."] }])).unwrap());
    assert!(a.failed.is_empty(), "{:?}", a.failed);
    // Lines of two rows: one suggestion per row.
    let two = json!([{ "paragraph_ids": [2, 3], "options": [[{ "kind": "Character", "text": "CESAR" }, { "kind": "Dialogue", "text": "Mañana." }]] }]);
    let errors = failed(p.suggest("t.review.txt", two).unwrap());
    assert!(errors[0].starts_with("¶2, ¶3 are in row 3 (¶2) and row 4 (¶3): "), "{errors:?}");
    // insert_after is refused on a version.
    let insert = json!([{ "insert_after": 3, "options": [[{ "kind": "Dialogue", "text": "Sí." }]] }]);
    let errors = failed(p.suggest("t.review.txt", insert).unwrap());
    assert!(errors[0].contains("never insert_after"), "{errors:?}");
}

#[test]
fn a_row_of_several_lines_names_them_all() {
    let mut p = Pair::new();
    // Rows 3 and 4 joined: CESAR and his line on both sides.
    let joined = p.links.rows.remove(3);
    for (m, cell) in joined.cells.into_iter().enumerate() {
        p.links.rows[2].cells[m].extend(cell);
    }
    let errors = failed(p.suggest("t.review.txt", json!([{ "paragraph_id": 3, "options": ["Mañana."] }])).unwrap());
    assert!(errors[0].starts_with("¶3 is in row 3 (¶2, ¶3): "), "{errors:?}");
    let errors =
        failed(p.suggest("t.review.txt", json!([{ "row": 3, "paragraph_id": 3, "options": ["Mañana."] }])).unwrap());
    assert!(errors[0].contains("give paragraph_ids [2, 3]"), "{errors:?}");
}

#[test]
fn new_version_is_for_the_host_unless_a_pair_says_otherwise() {
    let p = Pair::new();
    let args = json!({ "language": "es", "original_language": "en" });
    let want = Outcome::NewVersion(NewVersion { language: "es".into(), original_language: "en".into() });
    assert_eq!(p.outcome(None, "new_version", args.clone()).unwrap(), want);
    // On the original of a pair in that language: the host finds it again.
    assert_eq!(p.outcome(Some(Ok(p.links_ctx(0, &[]))), "new_version", args.clone()).unwrap(), want);
    // On the version itself: its path.
    let text = p.call(Some(Ok(p.links_ctx(1, &[]))), "new_version", args.clone()).unwrap();
    assert!(text.contains("is already the version (Español)") && text.ends_with("/films/film.es.fdx"), "{text}");
    // Another language: one version per script.
    let fr = json!({ "language": "fr", "original_language": "en" });
    let e = p.outcome(Some(Ok(p.links_ctx(0, &[]))), "new_version", fr).unwrap_err().to_string();
    assert!(e.contains("already has a version, film.es.fdx (Español), at /films/film.es.fdx"), "{e}");
    // A links file naming it that can't be used: never a second pair.
    let e = p.outcome(Some(Err("film.es.links.txt, line 3: bad row".into())), "new_version", args).unwrap_err();
    assert!(e.to_string().starts_with("film.es.links.txt, line 3: bad row. A links file naming"), "{e}");
    let bad = json!({ "language": "../x", "original_language": "en" });
    assert!(matches!(p.outcome(None, "new_version", bad), Err(ToolError::Args(_))));
    let missing = json!({ "language": "es" });
    assert!(matches!(p.outcome(None, "new_version", missing), Err(ToolError::Args(_))));
}

#[test]
fn the_pair_is_laid_out_for_the_calls_that_read_it() {
    for name in ["list_versions", "read_rows", "new_version", "suggest_change", "suggest_changes"] {
        assert!(uses_links(name), "{name}");
    }
    assert!(!uses_links("read_scene"));
    assert!(writes("new_version") && !may_append("new_version"));
    assert!(writes("suggest_changes") && !writes("read_rows"));
}

#[test]
fn new_version_messages() {
    let m = new_version_message("made film.es.fdx (Español)", "/films/film.es.fdx", true);
    assert_eq!(m.lines().nth(1), Some("The version: /films/film.es.fdx"));
    assert!(m.contains("pass this path as `script`"), "{m}");
    let m = new_version_message("made film.es.fdx (Español)", "/films/film.es.fdx", false);
    assert!(m.contains("the writer opens film.es.fdx in Scriptorium"), "{m}");
    let line = already_made("film.es.fdx", "film.es.links.txt", &["film.fdx changed: 2 rows found again.".into()]);
    assert_eq!(
        line,
        "film.es.fdx was already made, with film.es.links.txt. film.fdx changed: 2 rows found again. read_rows \
         with needs on it lists the rows left to translate."
    );
}
