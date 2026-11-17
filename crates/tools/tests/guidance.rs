//! The guidance agents get (instructions, the `guide` tool, description
//! hints, prompts): sizes, tool names, and one text for the guide and the
//! prompts.

use scriptorium_tools::guidance::{INSTRUCTIONS, Topic, guide, prompt, prompts};
use scriptorium_tools::prompt::unknown_tools;
use scriptorium_tools::{Offer, ToolError, catalogue, mcp_tool, offered, read_only, writes};
use serde_json::{Value, json};

/// snake_case words the guidance uses that aren't tools or arguments:
/// result fields and statuses, and `where_is_writer` (step C3).
const NOT_TOOLS: &[&str] = &[
    "where_is_writer",
    "is_version",
    "next_from_row",
    "in_sync",
    "this_changed",
    "other_changed",
    "both_changed",
    "one_sided",
];

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
    assert_eq!(spec.schema["properties"]["topic"]["enum"], json!(["review", "translate"]));
    assert!(read_only("guide") && !writes("guide"));
    assert_eq!(mcp_tool(&spec)["annotations"], json!({ "readOnlyHint": true }));
    assert!(spec.description.chars().count() < 4096);
}

#[test]
fn guide_returns_each_topic_in_full() {
    assert_eq!(guide(&json!({ "topic": "review" })).unwrap(), Topic::Review.text());
    assert_eq!(guide(&json!({ "topic": "translate" })).unwrap(), Topic::Translate.text());
    assert!(Topic::Review.text().starts_with("# Reviewing"));
    assert!(Topic::Translate.text().starts_with("# Translating"));
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
    let out = scriptorium_tools::call(&ctx, "guide", &json!({ "topic": "translate" })).unwrap();
    assert_eq!(out, scriptorium_tools::Outcome::Text(Topic::Translate.text().to_string()));
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
