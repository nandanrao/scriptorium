//! `write_script` with structural changes (plan A2): removing, inserting and
//! retyping paragraphs, spliced into the original bytes.

use scriptorium_core::doc::Group;
use scriptorium_core::{Document, ElementKind, NewPara, ParaId, Paragraph, Pos, Script, Selection, Span, Style};
use scriptorium_fdx::{FdxError, FdxFile, StructureReason};

/// Ids: 0 heading (Number, scene summary), 1 action, 2 character, 3 dialogue
/// (styled), 4 action, 5 action with minority layout and font.
const FILM: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">

  <Content>
    <Paragraph Number="1" Type="Scene Heading">
      <SceneProperties Length="1/8" Page="1" Title="">
        <Summary>
          <Paragraph Alignment="Left">
            <Text Font="Courier">They argue.</Text>
          </Paragraph>
        </Summary>
      </SceneProperties>
      <Text Font="Courier Final Draft" Size="12">INT. KITCHEN - DAY</Text>
    </Paragraph>
    <Paragraph Alignment="Left" FirstIndent="0.00" LeftIndent="1.50" Type="Action">
      <Text Font="Courier Final Draft" Size="12">Maria cooks.</Text>
    </Paragraph>
    <Paragraph Type="Character">
      <Text Font="Courier Final Draft" Size="12">MARIA</Text>
    </Paragraph>
    <Paragraph Alignment="Left" LeftIndent="2.50" RightIndent="6.00" Type="Dialogue" StartsNewPage="No">
      <Text Font="Courier Final Draft" Size="12">You </Text>
      <Text Font="Courier Final Draft" Size="12" Style="Italic">lied</Text>
      <Text Font="Courier Final Draft" Size="12">!</Text>
    </Paragraph>
    <Paragraph Alignment="Left" FirstIndent="0.00" LeftIndent="1.50" Type="Action">
      <Text Font="Courier Final Draft" Size="12">Tomas laughs.</Text>
    </Paragraph>
    <Paragraph Alignment="Left" FirstIndent="0.50" LeftIndent="1.00" Number="9" Type="Action">
      <Text Font="Courier" Size="10">He leaves.</Text>
    </Paragraph>
  </Content>
  <TitlePage>
    <Content>
      <Paragraph Type="Action">
        <Text>TITLE</Text>
      </Paragraph>
    </Content>
  </TitlePage>
</FinalDraft>
"#;

const NEW_ACTION_OPEN: &str = r#"<Paragraph Alignment="Left" FirstIndent="0.00" LeftIndent="1.50" Type="Action">"#;
const FONT: &str = r#" Font="Courier Final Draft" Size="12""#;

fn crlf(s: &str) -> String {
    s.replace('\n', "\r\n")
}

fn both(raw: &str) -> [String; 2] {
    [raw.to_string(), crlf(raw)]
}

/// Every line break in `s` is the file's: no bare `\n` in a CRLF file.
fn assert_eol(raw: &str, out: &str) {
    if raw.contains("\r\n") {
        assert_eq!(out.matches('\n').count(), out.matches("\r\n").count(), "bare \\n in CRLF output:\n{out}");
    } else {
        assert!(!out.contains('\r'));
    }
}

/// Converts `s` (written with `\n`) to `raw`'s line ending.
fn eol_of(raw: &str, s: &str) -> String {
    if raw.contains("\r\n") { crlf(s) } else { s.to_string() }
}

fn para(id: u64, kind: &str, text: &str) -> Paragraph {
    Paragraph::plain(ParaId(id), ElementKind::from_name(kind), text)
}

fn kinds_texts(s: &Script) -> Vec<(String, String)> {
    s.paragraphs.iter().map(|p| (p.kind.name().to_string(), p.text())).collect()
}

/// Write `script`, check the output re-parses to its kinds and texts and
/// uses the file's line ending, and return it.
fn write(f: &FdxFile, script: &Script) -> String {
    let out = f.write_script(script).unwrap();
    let back = FdxFile::parse(out.clone()).unwrap().to_script();
    assert_eq!(kinds_texts(&back), kinds_texts(script), "re-parsed output:\n{out}");
    assert_eol(f.raw(), &out);
    out
}

fn with(f: &FdxFile, edit: impl FnOnce(&mut Vec<Paragraph>)) -> Script {
    let mut s = f.to_script();
    edit(&mut s.paragraphs);
    s
}

/// The `<Paragraph>` element with id `i` in `raw`, with the line break and
/// indentation before it.
fn block(f: &FdxFile, i: usize) -> String {
    let text = f.paragraphs()[i].text();
    let at = f.raw().find(&text).unwrap();
    // The top-level paragraph (indented four spaces), not a scene summary's.
    let start = f.raw()[..at].rfind("\n    <Paragraph ").unwrap() + 1;
    let end = at + f.raw()[at..].find("</Paragraph>").unwrap() + "</Paragraph>".len();
    let ws = f.raw()[..start].trim_end_matches(' ').trim_end_matches('\n').trim_end_matches('\r').len();
    f.raw()[ws..end].to_string()
}

#[test]
fn no_op_is_byte_identical() {
    for raw in both(FILM).into_iter().chain(both(&dual_film())) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        assert_eq!(f.write_script(&f.to_script()).unwrap(), raw);
    }
}

#[test]
fn to_script_sets_the_id_floor() {
    let f = FdxFile::parse(FILM).unwrap();
    assert_eq!(f.to_script().id_floor, 6);
}

#[test]
fn remove_first_middle_last_and_all_but_one() {
    for raw in both(FILM) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        for i in [0, 2, 5] {
            let out = write(&f, &with(&f, |ps| drop(ps.remove(i))));
            assert_eq!(out, raw.replacen(&block(&f, i), "", 1), "remove {i}");
        }
        let out = write(&f, &with(&f, |ps| ps.retain(|p| p.id == ParaId(3))));
        assert!(out.contains("<Content>") && !out.contains("Maria cooks") && out.contains("lied"));
        assert!(out.contains("TITLE"), "title page untouched");
    }
}

#[test]
fn removing_a_scene_heading_takes_its_scene_properties() {
    for raw in both(FILM) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        let out = write(&f, &with(&f, |ps| drop(ps.remove(0))));
        assert!(!out.contains("SceneProperties") && !out.contains("They argue"));
        assert!(out.contains(&eol_of(&raw, "  <Content>\n    <Paragraph Alignment")), "{out}");
    }
}

fn new_action(text: &str) -> String {
    format!("{NEW_ACTION_OPEN}\n      <Text{FONT}>{text}</Text>\n    </Paragraph>")
}

#[test]
fn insert_at_start_middle_and_end() {
    for raw in both(FILM) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        let x = |t| eol_of(&raw, &new_action(t));
        let nl = eol_of(&raw, "\n");
        let indent = "    ";

        // Middle: after paragraph 1, at its end.
        let out = write(&f, &with(&f, |ps| ps.insert(2, para(6, "Action", "Steam rises."))));
        assert_eq!(out.replacen(&format!("{nl}{indent}{}", x("Steam rises.")), "", 1), raw);

        // Start: before the first paragraph.
        let out = write(&f, &with(&f, |ps| ps.insert(0, para(6, "Action", "Darkness."))));
        assert_eq!(out.replacen(&format!("{}{nl}{indent}", x("Darkness.")), "", 1), raw);
        assert!(out.contains(&eol_of(&raw, "<Content>\n    <Paragraph Alignment")));

        // End: after the last, two at once, in order.
        let out = write(
            &f,
            &with(&f, |ps| {
                ps.push(para(6, "Action", "Fade."));
                ps.push(para(7, "Action", "Out."));
            }),
        );
        let added = format!("{nl}{indent}{}{nl}{indent}{}", x("Fade."), x("Out."));
        assert_eq!(out.replacen(&added, "", 1), raw);
    }
}

#[test]
fn insert_next_to_removed_paragraphs() {
    for raw in both(FILM) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        // Replace the middle paragraph (delete + insert at the same spot).
        let s = with(&f, |ps| ps[2] = para(6, "Character", "TOMAS"));
        let out = write(&f, &s);
        let char_xml =
            eol_of(&raw, &format!("<Paragraph Type=\"Character\">\n      <Text{FONT}>TOMAS</Text>\n    </Paragraph>"));
        assert_eq!(out, raw.replacen(&eol_of(&raw, "<Paragraph Type=\"Character\">\n      <Text Font=\"Courier Final Draft\" Size=\"12\">MARIA</Text>\n    </Paragraph>"), &char_xml, 1));
        // Replace the first and the last.
        let s = with(&f, |ps| {
            ps[0] = para(6, "Scene Heading", "EXT. GARDEN - NIGHT");
            ps[5] = para(7, "Action", "The end.");
        });
        write(&f, &s);
        // Remove everything, insert new: right after <Content>.
        let s = with(&f, |ps| *ps = vec![para(6, "Action", "Only."), para(7, "Action", "Two.")]);
        let out = write(&f, &s);
        assert!(
            out.contains(&eol_of(
                &raw,
                &format!("  <Content>\n    {}\n    {}\n  </Content>", new_action("Only."), new_action("Two."))
            )),
            "{out}"
        );
    }
}

#[test]
fn new_paragraphs_keep_styled_spans() {
    let f = FdxFile::parse(FILM).unwrap();
    let span =
        |text: &str, bold, italic, underline| Span { text: text.into(), style: Style { bold, italic, underline } };
    let mut p = para(6, "Dialogue", "");
    p.spans = vec![
        span("You ", false, false, false),
        span("really", true, true, false),
        span(" did & <so>", false, false, true),
    ];
    let out = write(&f, &with(&f, |ps| ps.insert(4, p.clone())));
    // Dialogue template: majority layout, no StartsNewPage; one run per span.
    let want = format!(
        "\n    <Paragraph Alignment=\"Left\" LeftIndent=\"2.50\" RightIndent=\"6.00\" Type=\"Dialogue\">\n      \
         <Text{FONT}>You </Text>\n      <Text{FONT} Style=\"Bold+Italic\">really</Text>\n      \
         <Text{FONT} Style=\"Underline\"> did &amp; &lt;so&gt;</Text>\n    </Paragraph>"
    );
    assert_eq!(out.replacen(&want, "", 1), FILM, "{out}");
    let back = FdxFile::parse(out).unwrap().to_script();
    assert_eq!(back.paragraphs[4].spans, p.spans);
}

#[test]
fn template_uses_the_majority_not_the_nearest_and_drops_other_attributes() {
    let f = FdxFile::parse(FILM).unwrap();
    // After paragraph 5 (minority layout, Number, other font).
    let out = write(&f, &with(&f, |ps| ps.push(para(6, "Action", "New."))));
    assert!(out.ends_with(&format!(
        "{}\n  </Content>\n  <TitlePage>{}",
        new_action("New."),
        FILM.split("<TitlePage>").nth(1).unwrap()
    )));
    // A type the file has no paragraph of: Type only, bare runs.
    let out = write(&f, &with(&f, |ps| ps.push(para(6, "Transition", "CUT TO:"))));
    assert!(
        out.contains(
            "\n    <Paragraph Type=\"Transition\">\n      <Text>CUT TO:</Text>\n    </Paragraph>\n  </Content>"
        )
    );
    // An empty new paragraph still gets a run.
    let mut empty = para(6, "Action", "");
    empty.spans.clear();
    let out = write(&f, &with(&f, |ps| ps.push(empty)));
    assert!(out.contains(&format!("{NEW_ACTION_OPEN}\n      <Text{FONT}></Text>\n    </Paragraph>\n  </Content>")));
}

#[test]
fn retype_rewrites_type_and_layout() {
    for raw in both(FILM) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        // Action → Dialogue: the dialogue template's layout replaces the action's.
        let out = write(&f, &with(&f, |ps| ps[4].kind = ElementKind::Dialogue));
        let old = r#"<Paragraph Alignment="Left" FirstIndent="0.00" LeftIndent="1.50" Type="Action">
      <Text Font="Courier Final Draft" Size="12">Tomas laughs."#;
        let new = r#"<Paragraph Type="Dialogue" Alignment="Left" LeftIndent="2.50" RightIndent="6.00">
      <Text Font="Courier Final Draft" Size="12">Tomas laughs."#;
        assert_eq!(out, raw.replacen(&eol_of(&raw, old), &eol_of(&raw, new), 1));

        // To a type without a template: layout removed; Number (not layout) kept.
        let out = write(&f, &with(&f, |ps| ps[5].kind = ElementKind::Transition));
        assert!(out.contains(r#"<Paragraph Number="9" Type="Transition">"#), "{out}");

        // Retype and text edit together.
        let out = write(
            &f,
            &with(&f, |ps| {
                ps[2].kind = ElementKind::Action;
                ps[2].spans[0].text = "Maria waits.".into();
            }),
        );
        assert!(out.contains(&eol_of(&raw, &format!("<Paragraph Type=\"Action\" Alignment=\"Left\" FirstIndent=\"0.00\" LeftIndent=\"1.50\">\n      <Text{FONT}>Maria waits.</Text>"))), "{out}");
    }
}

#[test]
fn retype_from_scene_heading_drops_number_and_scene_properties() {
    for raw in both(FILM) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        let out = write(&f, &with(&f, |ps| ps[0].kind = ElementKind::Action));
        let want = eol_of(
            &raw,
            &format!(
                "  <Content>\n    <Paragraph Type=\"Action\" Alignment=\"Left\" FirstIndent=\"0.00\" LeftIndent=\"1.50\">\n      <Text{FONT}>INT. KITCHEN - DAY</Text>\n    </Paragraph>\n"
            ),
        );
        assert!(out.contains(&want), "{out}");
        assert!(!out.contains("SceneProperties") && !out.contains("Number=\"1\""));
        // Back to a heading: the Number isn't invented.
        let f2 = FdxFile::parse(out.clone()).unwrap();
        let out2 = write(&f2, &with(&f2, |ps| ps[0].kind = ElementKind::SceneHeading));
        assert!(out2.contains("<Paragraph Type=\"Scene Heading\">"), "{out2}");
    }
}

#[test]
fn edited_text_uses_the_files_line_ending() {
    let raw = crlf(FILM);
    let f = FdxFile::parse(raw.clone()).unwrap();
    // Three runs rewritten into a different number of runs.
    let out = write(&f, &with(&f, |ps| ps[3].spans = vec![Span { text: "Liar!".into(), style: Style::default() }]));
    assert_eol(&raw, &out);
    let out = f.write_edits(&[(3, "You did lie!".to_string())].into());
    assert_eol(&raw, &out);
}

#[test]
fn reordering_is_refused() {
    let f = FdxFile::parse(FILM).unwrap();
    let err = f.write_script(&with(&f, |ps| ps.swap(1, 4))).unwrap_err();
    // Order 0 4 2 3 1 5: 2 is the first to come after a later line.
    assert_eq!(err, FdxError::StructureChanged { reason: StructureReason::Moved, para: Some(ParaId(2)) });
    assert_eq!(err.to_string(), "cannot write this change to Final Draft: paragraphs were moved");
    let err = f.write_script(&with(&f, |ps| ps.insert(3, ps[3].clone()))).unwrap_err();
    assert_eq!(err, FdxError::StructureChanged { reason: StructureReason::Duplicated, para: Some(ParaId(3)) });
}

#[test]
fn removing_then_restoring_a_line_with_its_id_gives_back_the_file() {
    for raw in both(FILM) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        for id in [0, 2, 3, 5] {
            let mut doc = Document::new(f.to_script());
            let sel = Selection::cursor(Pos::new(ParaId(1), 0));
            let gone = doc.script().paragraphs[id as usize].clone();
            doc.edit(sel, Group::Other, |e| e.remove_para(ParaId(id)).map(|()| (sel, ()))).unwrap();
            assert_ne!(write(&f, doc.script()), raw);
            let restored = NewPara { kind: gone.kind.clone(), spans: gone.spans.clone() };
            doc.edit(sel, Group::Other, |e| {
                e.insert_para_with_id(id as usize, ParaId(id), restored).map(|()| (sel, ()))
            })
            .unwrap();
            assert_eq!(write(&f, doc.script()), raw, "restored paragraph {id}");
        }
    }
}

#[test]
fn ids_from_the_floor_up_are_new() {
    let f = FdxFile::parse(FILM).unwrap();
    // Same text as paragraph 5, but id 6: a new paragraph, the original stays.
    let out = write(&f, &with(&f, |ps| ps.push(para(6, "Action", "He leaves."))));
    assert_eq!(out.matches("He leaves.").count(), 2);

    // Cut the last original, rebase, insert: the new line doesn't reuse id 5.
    let mut doc = Document::new(f.to_script());
    let sel = Selection::cursor(Pos::new(ParaId(0), 0));
    doc.edit(sel, Group::Other, |e| e.remove_para(ParaId(5)).map(|()| (sel, ()))).unwrap();
    let mut doc = doc.rebase();
    let id = doc
        .edit(sel, Group::Other, |e| {
            e.insert_para(5, NewPara::plain(ElementKind::Action, "Later.")).map(|id| (sel, id))
        })
        .unwrap();
    assert_eq!(id, ParaId(6));
    let out = write(&f, doc.script());
    assert!(!out.contains("He leaves.") && out.contains("Later."));
}

// ---- DualDialogue --------------------------------------------------------

fn inner(kind: &str, text: &str) -> String {
    format!("        <Paragraph Type=\"{kind}\">\n          <Text>{text}</Text>\n        </Paragraph>\n")
}

fn dual(members: &[(&str, &str)]) -> String {
    let m: String = members.iter().map(|(k, t)| inner(k, t)).collect();
    format!("    <Paragraph>\n      <DualDialogue>\n{m}      </DualDialogue>\n    </Paragraph>\n")
}

fn plain(kind: &str, text: &str) -> String {
    format!("    <Paragraph Type=\"{kind}\">\n      <Text>{text}</Text>\n    </Paragraph>\n")
}

fn film_of(body: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<FinalDraft Version=\"5\">\n  <Content>\n{}  </Content>\n</FinalDraft>\n",
        body.concat()
    )
}

/// 0 heading; 1–2 wrapper 0; 3–4 wrapper 1 (adjacent); 5 action.
fn dual_film() -> String {
    film_of(&[
        plain("Scene Heading", "INT. KITCHEN - DAY"),
        dual(&[("Character", "MARIA"), ("Dialogue", "Go!")]),
        dual(&[("Character", "TOMAS"), ("Dialogue", "Stay!")]),
        plain("Action", "They stare."),
    ])
}

#[test]
fn dual_dialogue_structural_changes_are_refused() {
    let f = FdxFile::parse(dual_film()).unwrap();
    let refused = |s: Script, id: u64| {
        let err = f.write_script(&s).unwrap_err();
        assert_eq!(err, FdxError::StructureChanged { reason: StructureReason::InDualDialogue, para: Some(ParaId(id)) });
        assert!(err.to_string().contains("inside dual dialogue"), "{err}");
        assert_eq!(f.check_writable(&s).unwrap_err(), err);
    };
    refused(with(&f, |ps| drop(ps.remove(2))), 2);
    refused(with(&f, |ps| ps[1].kind = ElementKind::Action), 1);
    refused(with(&f, |ps| ps.insert(2, para(6, "Dialogue", "Wait."))), 6);
    // Text edits are fine.
    let out = write(&f, &with(&f, |ps| ps[2].spans[0].text = "Go now!".into()));
    assert_eq!(out.replace("Go now!", "Go!"), dual_film());
}

#[test]
fn inserting_around_dual_dialogue_goes_outside_the_wrapper() {
    for raw in both(&dual_film()) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        let nl = eol_of(&raw, "\n");
        let new = |t: &str| format!("<Paragraph Type=\"Action\">\n      <Text>{t}</Text>\n    </Paragraph>");
        // After a wrapper's last line, between two wrappers.
        let out = write(&f, &with(&f, |ps| ps.insert(3, para(6, "Action", "Pause."))));
        assert!(
            out.contains(&eol_of(
                &raw,
                &format!("</DualDialogue>\n    </Paragraph>\n    {}\n    <Paragraph>", new("Pause."))
            )),
            "{out}"
        );
        assert_eq!(out.replacen(&format!("{nl}    {}", eol_of(&raw, &new("Pause."))), "", 1), raw);
        // At the start when the first surviving original is inside a wrapper.
        let s = with(&f, |ps| {
            ps.remove(0);
            ps.insert(0, para(6, "Action", "First."));
        });
        let out = write(&f, &s);
        assert!(
            out.contains(&eol_of(
                &raw,
                &format!("<Content>\n    {}\n    <Paragraph>\n      <DualDialogue>", new("First."))
            )),
            "{out}"
        );
    }
}

// ---- Document end to end ----------------------------------------------------

#[test]
fn document_edits_write_and_reparse() {
    for raw in both(FILM).into_iter().chain(both(&dual_film())) {
        let f = FdxFile::parse(raw.clone()).unwrap();
        let mut doc = Document::new(f.to_script());
        let last = ParaId(f.paragraphs().len() as u64 - 1);
        let sel = Selection::cursor(Pos::new(ParaId(0), 0));
        doc.edit(sel, Group::Other, |e| {
            // Insert two lines after the heading, retype one and join them;
            // split the last line in the middle and join a new line onto the
            // tail; retype the last original; remove the heading.
            let a = e.insert_para(1, NewPara::plain(ElementKind::Action, "A new line."))?;
            e.set_kind(a, ElementKind::Dialogue)?;
            let b = e.insert_para(2, NewPara::plain(ElementKind::Action, " Joined."))?;
            e.join(a, b)?;
            let tail = e.split(Pos::new(last, 4))?;
            let n = e.doc().paragraphs().len();
            let c = e.insert_para(n, NewPara::plain(ElementKind::Action, " (end)"))?;
            e.join(tail, c)?;
            e.set_kind(last, ElementKind::Shot)?;
            e.remove_para(ParaId(0))?;
            Ok((sel, ()))
        })
        .unwrap();
        let out = write(&f, doc.script());
        let back = FdxFile::parse(out).unwrap();
        assert_eq!(kinds_texts(&back.to_script()), kinds_texts(doc.script()));
        // The untouched originals are still there byte for byte.
        let plain_film = !raw.contains("DualDialogue");
        assert!(!plain_film || back.raw().contains(&block(&f, 1).trim_start().to_string()));
    }
}
