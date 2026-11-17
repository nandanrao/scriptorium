//! The two offers: the chat's catalogue stays as threads hashed it, and the
//! agents' has every tool with `script`, annotations and a sane description.

use std::collections::HashSet;

use scriptorium_agent::file::tools_hash;
use scriptorium_tools::{
    AGENT_ONLY, Offer, ToolError, annotations, catalogue, check_args, mcp_tool, offered, read_only, writes,
};
use serde_json::json;

/// `tools_hash(&catalogue(Offer::Chat))` before the offers existed. A
/// change here shows every thread the "older tools" notice: only change it
/// on purpose, with the tools.
const CHAT_HASH: &str = "db1ffea00c0fd203";

#[test]
fn the_chat_catalogue_is_unchanged() {
    assert_eq!(tools_hash(&catalogue(Offer::Chat)), CHAT_HASH);
}

#[test]
fn the_chat_catalogue_has_no_agent_only_tool_or_host_field() {
    for spec in catalogue(Offer::Chat) {
        assert!(!AGENT_ONLY.contains(&spec.name.as_str()), "{}", spec.name);
        let props = spec.schema["properties"].as_object().unwrap();
        for field in ["script", "review_file", "before"] {
            assert!(!props.contains_key(field), "{}: {field}", spec.name);
        }
    }
}

#[test]
fn names_are_unique_in_each_offer() {
    for offer in [Offer::Chat, Offer::Agent] {
        let specs = catalogue(offer);
        let names: HashSet<_> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), specs.len(), "{offer:?}");
    }
}

#[test]
fn the_agent_offer_is_the_chat_offer_then_the_agent_only_tools() {
    let chat: Vec<String> = catalogue(Offer::Chat).into_iter().map(|s| s.name).collect();
    let agent: Vec<String> = catalogue(Offer::Agent).into_iter().map(|s| s.name).collect();
    let mut expected = chat.clone();
    expected.extend(AGENT_ONLY.iter().map(|s| s.to_string()));
    expected.push("guide".into());
    assert_eq!(agent, expected);
}

#[test]
fn every_agent_tool_takes_script() {
    for spec in catalogue(Offer::Agent) {
        // `guide` is the one tool without it.
        if spec.name == "guide" {
            continue;
        }
        assert!(spec.schema["properties"]["script"].is_object(), "{}", spec.name);
        // Left out, the host uses the script open in the app.
        let required = spec.schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|r| r == "script"), "{}: script required", spec.name);
        assert!(
            spec.schema["properties"]["script"]["description"]
                .as_str()
                .unwrap()
                .starts_with("Leave out to use the script open in Scriptorium; otherwise its absolute path"),
            "{}",
            spec.name
        );
    }
}

#[test]
fn the_agent_host_fields_are_applied() {
    let get = |name: &str| offered(Offer::Agent, name).unwrap();
    for name in ["suggest_change", "suggest_changes"] {
        let d = get(name).schema["properties"]["review_file"]["description"].as_str().unwrap().to_string();
        assert!(d.contains("Always give one"), "{name}: {d}");
    }
    let one = get("suggest_change");
    assert!(one.schema["properties"]["before"].is_object());
    assert!(!one.schema["required"].as_array().unwrap().iter().any(|r| r == "paragraph_id"));
    assert!(get("read_scene").schema["properties"].get("review_file").is_none());
    assert!(get("suggest_changes").schema["properties"].get("before").is_none());
}

#[test]
fn every_agent_tool_has_annotations_and_read_only_is_right() {
    let reads = [
        "script_info",
        "list_paragraphs",
        "read_scene",
        "read_script",
        "list_reviews",
        "list_suggestions",
        "list_versions",
        "read_rows",
        "guide",
    ];
    for spec in catalogue(Offer::Agent) {
        let name = spec.name.as_str();
        let a = annotations(name).unwrap_or_else(|| panic!("{name} has no annotations"));
        let ro = reads.contains(&name);
        assert_eq!(a["readOnlyHint"], json!(ro), "{name}");
        assert_eq!(read_only(name), ro, "{name}");
        assert_eq!(writes(name), !ro, "{name}: a tool either reads or writes");
        if !ro {
            assert_eq!(a["destructiveHint"], json!(name == "write_script"), "{name}");
        }
        let tool = mcp_tool(&spec);
        assert_eq!(tool["annotations"], a, "{name}");
        assert_eq!(tool["inputSchema"], spec.schema, "{name}");
        assert_eq!(tool["name"], json!(name));
    }
    assert_eq!(annotations("no_such_tool"), None);
    assert!(!read_only("no_such_tool") && !writes("no_such_tool"));
}

#[test]
fn every_description_is_under_4096_characters() {
    for spec in catalogue(Offer::Agent) {
        assert!(!spec.description.is_empty(), "{}", spec.name);
        assert!(spec.description.chars().count() < 4096, "{}: {}", spec.name, spec.description.len());
        assert_eq!(spec.schema["type"], "object", "{}", spec.name);
        assert_eq!(spec.schema["additionalProperties"], false, "{}", spec.name);
    }
}

#[test]
fn an_agent_only_tool_is_not_offered_to_the_chat() {
    for name in AGENT_ONLY {
        assert_eq!(offered(Offer::Chat, name).err(), Some(ToolError::Unknown(name.to_string())));
        assert!(offered(Offer::Agent, name).is_ok(), "{name}");
    }
    assert!(offered(Offer::Agent, "no_such_tool").is_err());
}

#[test]
fn check_args_holds_each_offer_to_its_own_fields() {
    let chat = offered(Offer::Chat, "suggest_change").unwrap();
    let agent = offered(Offer::Agent, "suggest_change").unwrap();
    let with_script =
        json!({ "script": "/s/film.fdx", "review_file": "x.review.txt", "paragraph_id": 1, "options": ["x"] });
    assert!(check_args(&chat, &with_script).is_err());
    assert_eq!(check_args(&agent, &with_script), Ok(()));
    let decide = offered(Offer::Agent, "decide_suggestion").unwrap();
    let rewrite =
        json!({ "script": "/s/f.fdx", "n": "e3", "decision": "rewrite", "lines": [{ "kind": "Action", "text": "x" }] });
    assert_eq!(check_args(&decide, &rewrite), Ok(()));
    let extra = json!({ "script": "/s/f.fdx", "n": 1, "decision": "keep", "review_file": "x.review.txt" });
    assert!(check_args(&decide, &extra).unwrap_err().to_string().contains("`review_file`"));
    let bad_line = json!({ "script": "/s/f.fdx", "n": 1, "decision": "rewrite", "lines": [{ "kind": "Action", "text": "x", "x": 1 }] });
    assert!(check_args(&decide, &bad_line).unwrap_err().to_string().contains("`lines[0].x`"));
}
