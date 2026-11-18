//! The two offers: the chat's catalogue stays as threads hashed it, and the
//! agents' has every tool with `script`, annotations and a sane description.

use std::collections::HashSet;

use scriptorium_agent::file::tools_hash;
use scriptorium_tools::{
    AGENT_ONLY, APP_TOOLS, OPEN_APP_ON_FILES, Offer, ToolError, annotations, catalogue, check_args, mcp_tool, offered,
    read_only, writes,
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
    for offer in [Offer::Chat, Offer::Agent, Offer::App] {
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
    expected.push("where_is_writer".into());
    expected.push("list_apps".into());
    expected.push("open_app".into());
    assert_eq!(agent, expected);
}

#[test]
fn every_agent_tool_takes_script() {
    for spec in catalogue(Offer::Agent) {
        // `guide` and `where_is_writer` are the tools without it.
        if spec.name == "guide" || spec.name == "where_is_writer" {
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
        "where_is_writer",
        "list_apps",
        "open_app",
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
        assert_eq!(a.get("openWorldHint").is_some(), name == "open_app", "{name}");
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

#[test]
fn where_is_writer_is_offered_to_agents_read_only_without_arguments() {
    assert!(offered(Offer::Chat, "where_is_writer").is_err());
    let spec = offered(Offer::Agent, "where_is_writer").unwrap();
    assert_eq!(spec.schema["properties"], json!({}));
    assert!(read_only("where_is_writer") && !writes("where_is_writer"));
    assert_eq!(mcp_tool(&spec)["annotations"], json!({ "readOnlyHint": true }));
    assert!(check_args(&spec, &json!({})).is_ok());
    assert!(check_args(&spec, &json!({ "script": "/a.fdx" })).is_err());
}

#[test]
fn the_app_offer_is_the_agent_offer_cut_to_the_app_tools_without_script() {
    let agent = catalogue(Offer::Agent);
    let app = catalogue(Offer::App);
    let names: Vec<&str> = app.iter().map(|s| s.name.as_str()).collect();
    // In the agents' order, every app tool and nothing else.
    let expected: Vec<&str> = agent.iter().map(|s| s.name.as_str()).filter(|n| APP_TOOLS.contains(n)).collect();
    assert_eq!(names, expected);
    assert_eq!(names.len(), APP_TOOLS.len());
    for spec in &app {
        let theirs = agent.iter().find(|s| s.name == spec.name).unwrap();
        assert_eq!(spec.description, theirs.description, "{}", spec.name);
        assert!(spec.schema["properties"].get("script").is_none(), "{}", spec.name);
        assert!(!spec.schema["required"].as_array().unwrap().iter().any(|r| r == "script"), "{}", spec.name);
        // Otherwise the agents' schema: `review_file` and `before` stay.
        let mut theirs = theirs.schema.clone();
        theirs["properties"].as_object_mut().unwrap().remove("script");
        theirs["required"].as_array_mut().unwrap().retain(|r| r != "script");
        assert_eq!(spec.schema, theirs, "{}", spec.name);
    }
    assert!(offered(Offer::App, "suggest_changes").unwrap().schema["properties"]["review_file"].is_object());
}

#[test]
fn an_app_is_offered_no_host_operation_guide_new_version_or_open_app() {
    for name in AGENT_ONLY.iter().copied().chain(["guide", "new_version", "open_app"]) {
        assert_eq!(offered(Offer::App, name).err(), Some(ToolError::Unknown(name.to_string())), "{name}");
    }
    for name in APP_TOOLS {
        assert!(offered(Offer::App, name).is_ok(), "{name}");
        assert!(offered(Offer::Agent, name).is_ok(), "{name}");
    }
}

#[test]
fn check_args_refuses_script_for_an_app() {
    for spec in catalogue(Offer::App) {
        let err = check_args(&spec, &json!({ "script": "/s/film.fdx" })).unwrap_err();
        assert!(err.to_string().contains("`script`"), "{}: {err}", spec.name);
    }
    let suggest = offered(Offer::App, "suggest_change").unwrap();
    let ok = json!({ "paragraph_id": 41, "options": ["new text"], "review_file": "x.review.txt" });
    assert_eq!(check_args(&suggest, &ok), Ok(()));
    let batch = offered(Offer::App, "suggest_changes").unwrap();
    let item_script = json!({ "suggestions": [{ "paragraph_id": 41, "options": ["x"], "script": "/a.fdx" }] });
    assert!(check_args(&batch, &item_script).is_err());
}

#[test]
fn list_apps_and_open_app_are_offered_to_agents() {
    let list = offered(Offer::Agent, "list_apps").unwrap();
    assert!(list.schema["properties"]["script"].is_object());
    assert_eq!(list.schema["required"], json!([]));
    assert!(list.description.contains("guide('app')"), "{}", list.description);
    assert!(read_only("list_apps") && !writes("list_apps"));
    assert_eq!(mcp_tool(&list)["annotations"], json!({ "readOnlyHint": true }));
    assert!(check_args(&list, &json!({ "script": "/s/film.fdx" })).is_ok());
    assert!(check_args(&list, &json!({ "name": "x" })).is_err());

    let open = offered(Offer::Agent, "open_app").unwrap();
    assert!(open.schema["properties"]["script"].is_object());
    assert_eq!(open.schema["required"], json!(["name"]));
    assert!(open.description.contains("guide('app')"), "{}", open.description);
    assert!(read_only("open_app") && !writes("open_app"));
    assert_eq!(mcp_tool(&open)["annotations"], json!({ "readOnlyHint": true, "openWorldHint": true }));
    assert!(check_args(&open, &json!({ "name": "scene-list", "script": "/s/film.fdx" })).is_ok());
    assert!(check_args(&open, &json!({ "name": "scene-list", "path": "x" })).is_err());

    // Neither is the chat's.
    for name in ["list_apps", "open_app"] {
        assert!(offered(Offer::Chat, name).is_err(), "{name}");
    }
    assert!(OPEN_APP_ON_FILES.contains("open_app again"));
}
