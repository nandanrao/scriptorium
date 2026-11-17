use std::collections::BTreeMap;

use scriptorium_core::{ElementKind, ParaId};
use scriptorium_versions::links::{self, Links, LinksError, Member, Para, Row, Stamp, language_name};

/// The example from docs/plans/versions.md, "The links file".
const EXAMPLE: &str = "\
SCRIPTORIUM LINKS v1
Id: lk-5k2m9q
a: PURPLE LIST.fdx
a language: en
a hash: 9f2c4e1a7b3d5e60
b: PURPLE LIST.es.fdx
b language: es
b hash: 0c4d2e8f11aa9b03
b created: version new

# One row per line: a ids | b ids. Under it, one line per paragraph: its
# kind, then its text when the row was last in sync (\"=\") or when it was
# made a placeholder (\"~\"). Neither: never in sync.
---
14 | 14
  a14 heading = INT. FERRY TERMINAL - NIGHT
  b14 heading ~ INT. FERRY TERMINAL - NIGHT
15 | 15 16
  a15 action = Rain hammers the glass. LUCÍA (40s) counts coins on a bench.
  b15 action = La lluvia golpea el cristal.
  b16 action = LUCÍA (cuarenta y tantos) cuenta monedas en un banco.
16 | 17
  a16 character = TOMÁS
  b17 character = TOMÁS
17 | 18
  a17 dialogue = You're not getting on that boat.
  b18 dialogue ~
18 |
  a18 dialogue = Watch me.
19 | 19
  a19 \"End of Act\" = END OF ACT ONE
  b19 \"End of Act\"
";

fn ids(v: &[u64]) -> Vec<ParaId> {
    v.iter().copied().map(ParaId).collect()
}

/// The example with its body replaced.
fn with_body(body: &str) -> String {
    let head = EXAMPLE.split("---\n").next().unwrap();
    format!("{head}---\n{body}")
}

fn parse_error(text: &str) -> (usize, String) {
    match links::parse(text) {
        Err(LinksError::Parse { line, message }) => (line, message),
        other => panic!("expected a parse error, got {other:?}"),
    }
}

fn para(kind: ElementKind, stamp: Option<Stamp>) -> Para {
    Para { kind, stamp }
}

#[test]
fn example_round_trips_byte_for_byte() {
    let l = links::parse(EXAMPLE).unwrap();
    assert_eq!(links::write(&l).unwrap(), EXAMPLE);

    assert_eq!(l.id, "lk-5k2m9q");
    assert_eq!(l.members.len(), 2);
    assert_eq!(l.members[0].file, "PURPLE LIST.fdx");
    assert_eq!(l.members[0].language.as_deref(), Some("en"));
    assert_eq!(l.members[1].hash, "0c4d2e8f11aa9b03");
    assert_eq!(l.members[0].created, None);
    assert_eq!(l.members[1].created.as_deref(), Some("version new"));
    assert_eq!(l.made(), Some(1));
    assert_eq!(l.member_of("purple list.ES.fdx"), Some(1));
    assert_eq!(l.rows.len(), 6);
    assert_eq!(l.rows[1].cells, vec![ids(&[15]), ids(&[15, 16])]);
    assert_eq!(l.rows[4].cells, vec![ids(&[18]), ids(&[])]);
    assert_eq!(
        l.paras[1][&ParaId(14)],
        para(ElementKind::SceneHeading, Some(Stamp::Placeholder("INT. FERRY TERMINAL - NIGHT".into())))
    );
    assert_eq!(l.paras[1][&ParaId(18)], para(ElementKind::Dialogue, Some(Stamp::Placeholder(String::new()))));
    assert_eq!(l.paras[0][&ParaId(18)], para(ElementKind::Dialogue, Some(Stamp::Synced("Watch me.".into()))));
    assert_eq!(l.paras[1][&ParaId(19)], para(ElementKind::Other("End of Act".into()), None));
    assert_eq!(l.members[1].label(), "Español");
}

#[test]
fn hand_edited_spacing_reads() {
    let text = "\u{feff}SCRIPTORIUM LINKS v1  \r\n\
         ID:lk-1\r\n\
         A :   one.fdx\r\n\
         a   HASH: 11\r\n\
         b: two.FDX\r\n\
         B Language:es\r\n\
         b hash:22\r\n\
         \r\n\
         ---\r\n\
         \r\n\
         # a comment\r\n\
         14|14\r\n\
         a14 Action=Hello   \r\n\
         \tb14   ACTION ~Hola\r\n\
         15   |   15    16\r\n\
         a15 \"End of Act\"\r\n\
         b15 dialogue\r\n\
         \x20\x20B16 dialogue   =\r\n";
    let l = links::parse(text).unwrap();
    assert_eq!(l.id, "lk-1");
    assert_eq!(l.members[0].file, "one.fdx");
    assert_eq!(l.members[0].hash, "11");
    assert_eq!(l.members[1].language.as_deref(), Some("es"));
    assert_eq!(l.rows[1].cells, vec![ids(&[15]), ids(&[15, 16])]);
    assert_eq!(l.paras[0][&ParaId(14)], para(ElementKind::Action, Some(Stamp::Synced("Hello".into()))));
    assert_eq!(l.paras[1][&ParaId(14)], para(ElementKind::Action, Some(Stamp::Placeholder("Hola".into()))));
    assert_eq!(l.paras[1][&ParaId(16)], para(ElementKind::Dialogue, Some(Stamp::Synced(String::new()))));
    // Written back in the canonical form, which reads the same.
    let written = links::write(&l).unwrap();
    assert!(written.contains("15 | 15 16\n  a15 \"End of Act\"\n  b15 dialogue\n  b16 dialogue =\n"), "{written}");
    assert_eq!(links::parse(&written).unwrap(), l);
}

#[test]
fn crossing_rows_are_refused_with_the_line() {
    let (line, message) = parse_error(&with_body("14 | 15\n15 | 14\n"));
    assert_eq!(line, 16);
    assert!(message.contains("rows cross"), "{message}");
}

#[test]
fn an_id_twice_is_refused_with_the_line() {
    let (line, message) = parse_error(&with_body("14 | 14\n15 | 15\n  a15 action = x\n15 | 16\n"));
    assert_eq!(line, 18);
    assert!(message.contains("a15 is listed twice"), "{message}");
    let (line, message) = parse_error(&with_body("14 | 14 14\n"));
    assert_eq!(line, 15);
    assert!(message.contains("b14 is listed twice"), "{message}");
}

#[test]
fn ids_out_of_order_in_a_row_are_refused_with_the_line() {
    let (line, message) = parse_error(&with_body("14 | 14\n15 | 16 15\n"));
    assert_eq!(line, 16);
    assert!(message.contains("out of order"), "{message}");
}

#[test]
fn bad_text_lines_are_refused_with_the_line() {
    let cases = [
        ("14 | 14\n  a14 action : nope\n", 16, "expected `=`, `~` or the end"),
        ("14 | 14\n  a = nope\n", 16, "expected a paragraph id"),
        ("14 | 14\n  c14 action = nope\n", 16, "no member `c`"),
        ("14 | 14\n  a15 action = not in the row\n", 16, "a15 isn't in the row above"),
        ("14 | 14\n  a14 action = one\n  a14 action = two\n", 17, "a14 has two text lines"),
        ("14 | 14\n  a14 = no kind\n", 16, "expected a14's kind"),
        ("14 | 14\n  a14 lyrics = x\n", 16, "unknown kind `lyrics`"),
        ("14 | 14\n  a14 \"End of Act = x\n", 16, "no closing"),
        ("14 | 14\n  a14 \"bad \\n escape\"\n", 16, "may follow"),
        ("14 | 14\n  a99999999999999999999 action\n", 16, "too large"),
    ];
    for (body, at, expected) in cases {
        let (line, message) = parse_error(&with_body(body));
        assert_eq!(line, at, "{body}");
        assert!(message.contains(expected), "{body}: {message}");
    }
    let (line, message) = parse_error(&with_body("  a14 action = before any row\n"));
    assert_eq!(line, 15);
    assert!(message.contains("under a row"), "{message}");
}

#[test]
fn every_listed_paragraph_needs_a_text_line() {
    let (line, message) = parse_error(&with_body("14 | 14\n  a14 action\n  b14 action\n15 | 15\n  a15 action\n"));
    assert_eq!(line, 18);
    assert!(message.contains("b15 has no text line"), "{message}");
}

#[test]
fn bad_row_lines_are_refused_with_the_line() {
    let cases = [
        ("14 | 14 | 14\n", "2 sides"),
        ("14 14\n", "2 sides"),
        ("14 | x\n", "`x` is not a paragraph id"),
        ("14 | 99999999999999999999\n", "is not a paragraph id"),
        ("|\n", "no ids"),
    ];
    for (body, expected) in cases {
        let (line, message) = parse_error(&with_body(body));
        assert_eq!(line, 15, "{body}");
        assert!(message.contains(expected), "{body}: {message}");
    }
}

#[test]
fn one_sided_rows() {
    let body = "14 | 14\n  a14 action\n  b14 action\n15 |\n  a15 action = New in English.\n| 15\n  b15 action = Solo en español.\n";
    let l = links::parse(&with_body(body)).unwrap();
    assert_eq!(l.rows[1].cells, vec![ids(&[15]), ids(&[])]);
    assert_eq!(l.rows[2].cells, vec![ids(&[]), ids(&[15])]);
    assert_eq!(links::write(&l).unwrap(), with_body(body));
}

#[test]
fn placeholder_with_and_without_text_and_empty_synced_text() {
    let body = "14 | 14\n  a14 action =\n  b14 action ~\n15 | 15\n  a15 action = Hi.\n  b15 action ~ Hi.\n";
    let l = links::parse(&with_body(body)).unwrap();
    assert_eq!(l.paras[0][&ParaId(14)].stamp, Some(Stamp::Synced(String::new())));
    assert_eq!(l.paras[1][&ParaId(14)].stamp, Some(Stamp::Placeholder(String::new())));
    assert_eq!(l.paras[1][&ParaId(15)].stamp, Some(Stamp::Placeholder("Hi.".into())));
    assert_eq!(links::write(&l).unwrap(), with_body(body));
}

#[test]
fn kinds_round_trip() {
    let all = [
        ElementKind::SceneHeading,
        ElementKind::Action,
        ElementKind::Character,
        ElementKind::Parenthetical,
        ElementKind::Dialogue,
        ElementKind::Transition,
        ElementKind::Shot,
        ElementKind::General,
        ElementKind::Other("End of Act".into()),
        ElementKind::Other("Lyrics".into()),
        ElementKind::Other("Action".into()),
        ElementKind::Other(String::new()),
        ElementKind::Other("say \"hi\" \\ there".into()),
    ];
    let mut l = links::parse(EXAMPLE).unwrap();
    l.rows = (0..all.len() as u64).map(|i| Row { cells: vec![ids(&[i]), ids(&[])] }).collect();
    l.paras[0] = all.iter().enumerate().map(|(i, k)| (ParaId(i as u64), para(k.clone(), None))).collect();
    l.paras[1].clear();
    let text = links::write(&l).unwrap();
    assert!(text.contains("  a12 \"say \\\"hi\\\" \\\\ there\"\n"), "{text}");
    assert_eq!(links::parse(&text).unwrap(), l);
}

#[test]
fn unknown_header_is_kept() {
    let text = EXAMPLE.replace("b created: version new\n", "b created: version new\nMade by: someone\nx note: hi\n");
    let l = links::parse(&text).unwrap();
    assert_eq!(
        l.extra_headers,
        vec![("Made by".to_string(), "someone".to_string()), ("x note".to_string(), "hi".to_string())]
    );
    assert_eq!(links::write(&l).unwrap(), text);
}

#[test]
fn header_errors() {
    let (line, m) = parse_error("SCRIPTORIUM LINKS v2\n");
    assert_eq!(line, 1);
    assert!(m.contains("v1"), "{m}");
    let (line, _) = parse_error("SCRIPT REVIEW\n");
    assert_eq!(line, 1);
    let (line, m) = parse_error(&EXAMPLE.replace("Id: lk-5k2m9q", "Id: not ok!"));
    assert_eq!(line, 2);
    assert!(m.contains("valid id"), "{m}");
    let (_, m) = parse_error(&EXAMPLE.replace("Id: lk-5k2m9q\n", ""));
    assert!(m.contains("Id:"), "{m}");
    let (line, m) = parse_error(&EXAMPLE.replace("b hash: 0c4d2e8f11aa9b03\n", ""));
    assert_eq!(line, 6);
    assert!(m.contains("b hash:"), "{m}");
    let (line, m) = parse_error(&EXAMPLE.replace("a language: en\n", "a language: en\na language: fr\n"));
    assert_eq!(line, 5);
    assert!(m.contains("second"), "{m}");
    let (_, m) = parse_error(&EXAMPLE.replace("---\n", ""));
    assert!(m.contains("---"), "{m}");
    let (line, m) = parse_error(&EXAMPLE.replace("a language: en", "a language en"));
    assert_eq!(line, 4);
    assert!(m.contains("key: value"), "{m}");
}

#[test]
fn member_errors_point_at_their_line() {
    let (line, m) = parse_error(&EXAMPLE.replace("b: PURPLE LIST.es.fdx", "b: purple list.FDX"));
    assert_eq!(line, 6);
    assert!(m.contains("named twice"), "{m}");
    let (line, m) = parse_error(&EXAMPLE.replace("a hash: 9f2c4e1a7b3d5e60\n", "a hash: 9f\na created: version new\n"));
    assert_eq!(line, 10);
    assert!(m.contains("only one"), "{m}");
}

#[test]
fn odd_and_empty_header_keys() {
    let (line, m) = parse_error(&EXAMPLE.replace("b created: version new\n", "b created: version new\n: value\n"));
    assert_eq!(line, 10);
    assert!(m.contains("no key"), "{m}");
    let (line, m) = parse_error(&EXAMPLE.replace("b created: version new\n", "b created: version new\nx: y.fdx\n"));
    assert_eq!(line, 10);
    assert!(m.contains("unknown member letter `x`"), "{m}");
    let (line, m) = parse_error(&EXAMPLE.replace("b created: version new\n", "b created: version new\nq hash: 1\n"));
    assert_eq!(line, 10);
    assert!(m.contains("unknown member letter `q`"), "{m}");
    // Odd but harmless keys are kept.
    let text = EXAMPLE.replace("b created: version new\n", "b created: version new\nweird   key--: v: w\n");
    let l = links::parse(&text).unwrap();
    assert_eq!(l.extra_headers, vec![("weird   key--".to_string(), "v: w".to_string())]);
    assert_eq!(links::write(&l).unwrap(), text);
}

#[test]
fn more_than_two_members_are_refused() {
    let text =
        EXAMPLE.replace("b created: version new\n", "b created: version new\nc: PURPLE LIST.fr.fdx\nc hash: 33\n");
    let (line, m) = parse_error(&text);
    assert_eq!(line, 10);
    assert!(m.contains("one pair"), "{m}");

    let mut l = links::parse(EXAMPLE).unwrap();
    l.members.push(Member { file: "c.fdx".into(), language: None, hash: "1".into(), created: None });
    l.paras.push(BTreeMap::new());
    for r in &mut l.rows {
        r.cells.push(vec![]);
    }
    let e = links::validate(&l).unwrap_err().to_string();
    assert!(e.contains("one pair"), "{e}");
}

#[test]
fn file_names_must_be_plain_fdx_names() {
    for bad in ["sub/x.fdx", "..\\x.fdx", "C:x.fdx", ".hidden.fdx", "x.txt", "x.review.txt"] {
        let (line, m) = parse_error(&EXAMPLE.replace("b: PURPLE LIST.es.fdx", &format!("b: {bad}")));
        assert_eq!(line, 6, "{bad}");
        assert!(m.contains(bad), "{bad}: {m}");
        let mut l = links::parse(EXAMPLE).unwrap();
        l.members[1].file = bad.to_string();
        assert!(links::validate(&l).is_err(), "{bad}");
    }
}

#[test]
fn validate_checks_hand_built_values() {
    let good = links::parse(EXAMPLE).unwrap();

    let mut l = good.clone();
    l.paras.pop();
    assert!(links::validate(&l).unwrap_err().to_string().contains("table per member"));

    let mut l = good.clone();
    l.rows[0].cells.push(vec![]);
    assert!(links::validate(&l).unwrap_err().to_string().contains("sides"));

    let mut l = good.clone();
    l.rows.push(Row { cells: vec![vec![], vec![]] });
    assert!(links::validate(&l).unwrap_err().to_string().contains("no ids"));

    let mut l = good.clone();
    l.paras[0].remove(&ParaId(14));
    assert!(links::validate(&l).unwrap_err().to_string().contains("a14 is listed but has no kind"));

    let mut l = good.clone();
    l.paras[0].insert(ParaId(99), para(ElementKind::Action, None));
    assert!(matches!(links::write(&l), Err(LinksError::Invalid(_))));

    let mut l = good.clone();
    l.paras[0].get_mut(&ParaId(14)).unwrap().stamp = Some(Stamp::Synced("two\nlines".into()));
    assert!(links::write(&l).is_err());

    let mut l = good.clone();
    l.paras[0].get_mut(&ParaId(14)).unwrap().kind = ElementKind::Other("a\nb".into());
    assert!(links::write(&l).is_err());

    let mut l = good.clone();
    l.rows.swap(0, 1);
    assert!(links::validate(&l).unwrap_err().to_string().contains("rows cross"));

    for key in ["a hash", "x", "", "a:b", "# c"] {
        let mut l: Links = good.clone();
        l.extra_headers.push((key.into(), "v".into()));
        assert!(links::write(&l).is_err(), "{key:?}");
    }
}

/// xorshift: enough randomness for mutation tests without a dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Property: any text that parses also writes, and the written text parses
/// to the same value (and writes to itself).
#[test]
fn whatever_parses_writes_and_reparses_equal() {
    let pieces = [
        "",
        " ",
        "  ",
        "|",
        " | ",
        "=",
        "~",
        ":",
        "#",
        "\"",
        "\\",
        "a",
        "b",
        "c",
        "x",
        "14",
        "15",
        "20",
        "99",
        "action",
        "dialogue",
        "\"End of Act\"",
        "Id",
        "hash",
        "language",
        "created",
        "---",
        "é",
        "\t",
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut parsed = 0;
    for _ in 0..20_000 {
        let mut lines: Vec<String> = EXAMPLE.lines().map(str::to_string).collect();
        for _ in 0..1 + rng.next(3) {
            let i = rng.next(lines.len());
            match rng.next(5) {
                0 => {
                    lines.remove(i);
                }
                1 => {
                    let j = rng.next(lines.len());
                    lines.swap(i, j);
                }
                2 => {
                    let l = lines[i].clone();
                    lines.insert(i, l);
                }
                _ => {
                    let line = &mut lines[i];
                    let mut at = rng.next(line.len() + 1);
                    while !line.is_char_boundary(at) {
                        at -= 1;
                    }
                    line.insert_str(at, pieces[rng.next(pieces.len())]);
                }
            }
        }
        let text = lines.join("\n") + "\n";
        if let Ok(l) = links::parse(&text) {
            parsed += 1;
            let written = links::write(&l).unwrap_or_else(|e| panic!("{e}\n{text}"));
            let again = links::parse(&written).unwrap_or_else(|e| panic!("{e}\n{written}"));
            assert_eq!(again, l, "{text}");
            assert_eq!(links::write(&again).unwrap(), written);
        }
    }
    assert!(parsed > 1000, "only {parsed} mutants parsed");
}

#[test]
fn language_names() {
    assert_eq!(language_name("en"), "English");
    assert_eq!(language_name("es"), "Español");
    assert_eq!(language_name("ES"), "Español");
    assert_eq!(language_name("es-MX"), "Español");
    assert_eq!(language_name("pt_BR"), "Português");
    assert_eq!(language_name("ca"), "Català");
    assert_eq!(language_name("tlh"), "tlh");
    assert_eq!(language_name("xx-YY"), "xx-YY");
}

#[test]
fn label_falls_back_to_the_file_stem() {
    let mut l = links::parse(EXAMPLE).unwrap();
    l.members[0].language = None;
    assert_eq!(l.members[0].label(), "PURPLE LIST");
    l.members[0].language = Some("xx".into());
    assert_eq!(l.members[0].label(), "xx");
}
