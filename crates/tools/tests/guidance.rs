//! The guidance agents get (instructions, the `guide` tool, description
//! hints, prompts): sizes, tool names, and one text for the guide and the
//! prompts.

use scriptorium_tools::guidance::{INSTRUCTIONS, Topic, guide, prompt, prompts};
use scriptorium_tools::prompt::unknown_tools;
use scriptorium_tools::{Offer, ToolError, catalogue, mcp_tool, offered, read_only, writes};
use serde_json::{Value, json};

/// snake_case words the guidance uses that aren't tools or arguments:
/// result fields and statuses.
const NOT_TOOLS: &[&str] =
    &["is_version", "next_from_row", "in_sync", "this_changed", "other_changed", "both_changed", "one_sided"];

fn unknown(text: &str) -> Vec<String> {
    unknown_tools(text, &catalogue(Offer::Agent)).into_iter().filter(|w| !NOT_TOOLS.contains(&w.as_str())).collect()
}

#[test]
fn the_instructions_fit_the_clients_caps() {
    assert!(INSTRUCTIONS.len() < 4096, "{}", INSTRUCTIONS.len());
    // Codex shows the first 512 characters: the first paragraph, standing alone.
    let first = INSTRUCTIONS.split("\n\n").next().unwrap();
    assert!(first.chars().count() <= 512, "{}", first.chars().count());
    assert!(first.ends_with('.'));
    for must in ["where_is_writer", "Propose, don't edit", "guide('review')", "guide('translate')", "feedback"] {
        assert!(first.contains(must), "{must}");
    }
    // Older Claude Code keeps the first 2 KB: every rule is in it.
    let head: String = INSTRUCTIONS.chars().take(2048).collect();
    for must in
        ["review_file", "Never edit the .fdx", "only when the writer asks", "undo", "ask the writer for its path"]
    {
        assert!(head.contains(must), "{must}");
    }
}

#[test]
fn the_guidance_names_only_offered_tools() {
    assert_eq!(unknown(INSTRUCTIONS), Vec::<String>::new());
    for topic in Topic::ALL {
        assert_eq!(unknown(topic.text()), Vec::<String>::new(), "{topic:?}");
    }
    for spec in catalogue(Offer::Agent) {
        assert_eq!(unknown(&spec.description), Vec::<String>::new(), "{}", spec.name);
    }
    // The check itself catches a tool that doesn't exist.
    assert_eq!(unknown("call suggest_all first"), vec!["suggest_all".to_string()]);
}

#[test]
fn the_guide_is_offered_read_only_without_script() {
    let spec = offered(Offer::Agent, "guide").unwrap();
    assert!(offered(Offer::Chat, "guide").is_err());
    assert!(spec.schema["properties"].get("script").is_none());
    assert_eq!(spec.schema["required"], json!(["topic"]));
    assert_eq!(spec.schema["properties"]["topic"]["enum"], json!(["review", "translate", "app"]));
    assert!(read_only("guide") && !writes("guide"));
    assert_eq!(mcp_tool(&spec)["annotations"], json!({ "readOnlyHint": true }));
    assert!(spec.description.chars().count() < 4096);
}

#[test]
fn guide_returns_each_topic_in_full() {
    assert_eq!(guide(&json!({ "topic": "review" })).unwrap(), Topic::Review.text());
    assert_eq!(guide(&json!({ "topic": "translate" })).unwrap(), Topic::Translate.text());
    assert_eq!(guide(&json!({ "topic": "app" })).unwrap(), Topic::App.text());
    assert!(Topic::Review.text().starts_with("# Reviewing"));
    assert!(Topic::Translate.text().starts_with("# Translating"));
    assert!(Topic::App.text().starts_with("# Building an app"));
    for bad in [json!({}), json!({ "topic": 1 }), json!({ "topic": "poetry" })] {
        assert!(matches!(guide(&bad), Err(ToolError::Args(_))), "{bad}");
    }
    for topic in Topic::ALL {
        assert_eq!(Topic::from_name(topic.name()), Some(topic));
    }
}

#[test]
fn the_guide_runs_through_call_like_the_other_tools() {
    // `call` needs a Ctx; the guide reads none of it, so any script will do.
    let fdx = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5"><Content>
<Paragraph Type="Action"><Text>Hello.</Text></Paragraph>
</Content></FinalDraft>"#;
    let out = with_ctx(fdx, |ctx| scriptorium_tools::call(ctx, "guide", &json!({ "topic": "translate" })).unwrap());
    assert_eq!(out, scriptorium_tools::Outcome::Text(Topic::Translate.text().to_string()));
}

/// Runs `f` with a `Ctx` on the script `fdx`.
fn with_ctx<T>(fdx: &str, f: impl FnOnce(&scriptorium_tools::Ctx) -> T) -> T {
    let fdx = scriptorium_fdx::FdxFile::parse(fdx.to_string()).unwrap();
    let doc = scriptorium_core::Document::new(fdx.to_script());
    let session = scriptorium_review::Session::default();
    let ctx = scriptorium_tools::Ctx {
        doc: &doc,
        session: &session,
        script_name: "film.fdx",
        is_caps: &|_| false,
        default_review: None,
        links: None,
    };
    f(&ctx)
}

/// The JS library apps load, as the apps crate serves it.
const LIBRARY: &str = include_str!("../../apps/assets/scriptorium.js");

#[test]
fn the_app_guide_names_only_what_the_library_has() {
    let text = Topic::App.text();
    // `Scriptorium.<name>` and `Scriptorium.data.<name>`.
    let exported = LIBRARY.split("window.Scriptorium = {").nth(1).unwrap().split('}').next().unwrap();
    let exported: Vec<&str> = exported.split(',').map(str::trim).collect();
    let data = LIBRARY.split("const data = {").nth(1).unwrap().split("\n  };").next().unwrap();
    let mut named = 0;
    for (i, _) in text.match_indices("Scriptorium.") {
        let rest = &text[i + "Scriptorium.".len()..];
        let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        if word.is_empty() {
            continue; // "… in Scriptorium." ends a sentence
        }
        if word == "data" && rest.starts_with("data.") {
            let sub: String = rest["data.".len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            assert!(data.contains(&format!("async {sub}(")), "Scriptorium.data.{sub}");
        } else {
            assert!(exported.contains(&word.as_str()), "Scriptorium.{word}");
        }
        named += 1;
    }
    assert!(named >= 10, "{named}");
    // The events it names are the ones the library passes on.
    for event in ["script", "writer", "decided", "open"] {
        assert!(text.contains(&format!("`\"{event}\"`")), "{event}");
        assert!(LIBRARY.contains(&format!("\"{event}\"")), "{event}");
    }
    // The tag it says to put first.
    assert!(text.contains(r#"<script src="/scriptorium.js"></script>"#));
}

#[test]
fn the_app_guide_lists_exactly_the_app_tools() {
    let text = Topic::App.text();
    let line = text.split("The tools an app may call: ").nth(1).unwrap().split(". ").next().unwrap();
    let named: Vec<&str> = line.split(',').map(str::trim).collect();
    assert_eq!(named, scriptorium_tools::APP_TOOLS);
}

#[test]
fn the_app_guide_shows_list_paragraphs_as_it_answers() {
    let line = Topic::App.text().lines().find(|l| l.starts_with(r#"{"total""#)).unwrap();
    let shown: Value = serde_json::from_str(line).unwrap();
    let fdx = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5"><Content>
<Paragraph Type="Scene Heading"><Text>INT. KITCHEN - NIGHT</Text></Paragraph>
<Paragraph Type="Character"><Text>ANNA</Text></Paragraph>
<Paragraph Type="Dialogue"><Text>Where were you?</Text></Paragraph>
</Content></FinalDraft>"#;
    let real =
        with_ctx(fdx, |ctx| scriptorium_tools::call(ctx, "list_paragraphs", &json!({ "kind": "Dialogue" })).unwrap());
    let scriptorium_tools::Outcome::Text(real) = real else { panic!("{real:?}") };
    let real: Value = serde_json::from_str(&real).unwrap();
    let keys = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&shown), keys(&real));
    assert_eq!(keys(&shown["paragraphs"][0]), keys(&real["paragraphs"][0]));
    // The same values too, but for the id.
    let (mut s, mut r) = (shown["paragraphs"][0].clone(), real["paragraphs"][0].clone());
    assert!(s["id"].is_u64() && r["id"].is_u64());
    s["id"] = json!(0);
    r["id"] = json!(0);
    assert_eq!(s, r);
    assert_eq!((shown["total"].clone(), shown["more"].clone()), (real["total"].clone(), real["more"].clone()));
}

#[test]
fn the_instructions_point_at_the_app_guide() {
    assert!(INSTRUCTIONS.contains("To build an app (a small web page on the script), call guide('app') first."));
    let spec = offered(Offer::Agent, "guide").unwrap();
    assert!(spec.description.contains("guide('app')"));
}

#[test]
fn the_descriptions_point_at_the_guide() {
    let desc = |name: &str| offered(Offer::Agent, name).unwrap().description;
    for name in ["suggest_change", "suggest_changes"] {
        assert!(desc(name).contains("guide('review')"), "{name}");
        assert!(desc(name).contains("review_file"), "{name}");
    }
    for name in ["new_version", "read_rows"] {
        assert!(desc(name).contains("guide('translate')"), "{name}");
    }
    // The chat's catalogue is untouched (its hash is checked in offer.rs).
    for spec in catalogue(Offer::Chat) {
        assert!(!spec.description.contains("guide("), "{}", spec.name);
    }
}

#[test]
fn the_prompts_send_the_guides_text() {
    let list = prompts();
    let names: Vec<&str> = list.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["review_pass", "translate"]);
    let args = |p: &Value| -> Vec<String> {
        p["arguments"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(args(&list[0]), ["scene"]);
    assert_eq!(args(&list[1]), ["language", "scene"]);

    let text = |v: &Value, i: usize| v["messages"][i]["content"]["text"].as_str().unwrap().to_string();
    let review = prompt("review_pass", &json!({ "scene": "3" })).unwrap();
    assert_eq!(text(&review, 0), Topic::Review.text());
    assert!(text(&review, 1).contains("scene 3"));
    let translate = prompt("translate", &json!({ "language": "Spanish" })).unwrap();
    assert_eq!(text(&translate, 0), Topic::Translate.text());
    assert!(text(&translate, 1).contains("into Spanish"));
    let bare = prompt("translate", &Value::Null).unwrap();
    assert!(text(&bare, 1).contains("ask me"));
    assert_eq!(prompt("nope", &Value::Null), Err(ToolError::Unknown("nope".into())));
}

#[test]
fn the_library_reads_where_is_writer_as_it_answers() {
    use scriptorium_core::ParaId;
    use scriptorium_tools::context::{Scene, Selected, Showing, ViewContext, text};
    // The library's patterns, as written in it.
    let paragraph = r"/^(?:Cursor: on|Selected:) ¶(\d+)/m";
    let scene = r"/^Scene: \d+, (.*) \(read_scene with scene \d+\)\.$/m";
    assert!(LIBRARY.contains(paragraph) && LIBRARY.contains(scene));

    let ctx = |selection: Option<Selected>, scene: Option<Scene>| ViewContext {
        script: "/films/film.fdx".into(),
        pair: None,
        view: Showing::Script,
        card: None,
        selection,
        scene,
        visible: None,
        unwritten: false,
    };
    let kitchen = || Some(Scene { number: 4, heading: "INT. KITCHEN (NIGHT) - DAY".into() });
    let lines = |t: &str| t.lines().map(str::to_string).collect::<Vec<_>>();

    // A cursor: "Cursor: on ¶41" starts a line; the scene line is whole.
    let cursor = Selected { first: ParaId(41), last: ParaId(41), text: "Hi.".into(), collapsed: true };
    let t = lines(&text(&ctx(Some(cursor), kitchen())));
    assert!(t.iter().any(|l| l.starts_with("Cursor: on ¶41,")), "{t:?}");
    assert!(t.iter().any(|l| l == "Scene: 4, INT. KITCHEN (NIGHT) - DAY (read_scene with scene 4)."), "{t:?}");

    // A selection: its first line.
    let sel = Selected { first: ParaId(41), last: ParaId(44), text: "a\nb".into(), collapsed: false };
    let t = lines(&text(&ctx(Some(sel), kitchen())));
    assert!(t.iter().any(|l| l.starts_with("Selected: ¶41 to ¶44")), "{t:?}");

    // No scene and no line: neither pattern finds anything.
    let t = lines(&text(&ctx(None, None)));
    assert!(!t.iter().any(|l| l.starts_with("Cursor: on ¶") || l.starts_with("Selected: ¶")), "{t:?}");
    assert!(!t.iter().any(|l| l.starts_with("Scene: ") && l.as_bytes()[7].is_ascii_digit()), "{t:?}");
}

#[test]
fn the_app_guide_names_the_tools_that_answer_json() {
    let text = Topic::App.text().split_whitespace().collect::<Vec<_>>().join(" ");
    let named = text.split("for the tools that answer JSON: ").nth(1).unwrap().split(". ").next().unwrap();
    let named: Vec<&str> = named.split(',').map(str::trim).collect();
    let json_tools = [
        "list_paragraphs",
        "list_reviews",
        "list_suggestions",
        "list_versions",
        "read_rows",
        "suggest_change",
        "suggest_changes",
    ];
    assert_eq!(named, json_tools);
    // Every other app tool answers text: those that run on a bare script here.
    let fdx = r#"<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5"><Content>
<Paragraph Type="Scene Heading"><Text>INT. KITCHEN - NIGHT</Text></Paragraph>
<Paragraph Type="Action"><Text>Rain.</Text></Paragraph>
</Content></FinalDraft>"#;
    with_ctx(fdx, |ctx| {
        let answer = |name: &str, args: Value| match scriptorium_tools::call(ctx, name, &args).unwrap() {
            scriptorium_tools::Outcome::Text(t) => t,
            other => panic!("{name}: {other:?}"),
        };
        for name in ["list_paragraphs", "list_reviews"] {
            assert!(serde_json::from_str::<Value>(&answer(name, json!({}))).is_ok(), "{name}");
        }
        for (name, args) in
            [("script_info", json!({})), ("read_scene", json!({ "scene": 1 })), ("read_script", json!({}))]
        {
            assert!(serde_json::from_str::<Value>(&answer(name, args)).is_err(), "{name}");
        }
    });
}
