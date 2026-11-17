//! Claude Desktop: `~/Library/Application Support/Claude/claude_desktop_config.json`,
//! `{"mcpServers": {"scriptorium": {"command": …, "args": […]}}}`. Desktop
//! keeps its own `preferences` key in the same file; it is kept as is.

use std::path::Path;

use serde_json::{Map, Value};

use crate::{Entry, Refused, State, json};

/// What the config says about the `scriptorium` server.
pub fn state(text: &str, this: &Path) -> State {
    json::state(text, this)
}

/// The config with our entry added, or ours updated in place.
pub fn merge(text: &str, entry: &Entry) -> Result<String, Refused> {
    json::merge(text, entry, |command, args| {
        let mut m = Map::new();
        m.insert("command".to_owned(), Value::String(command.to_owned()));
        m.insert("args".to_owned(), Value::Array(args.iter().cloned().map(Value::String).collect()));
        m
    })
}

/// The config without our entry (unchanged if it has none).
pub fn remove(text: &str, this: &Path) -> Result<String, Refused> {
    json::remove(text, this)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn this() -> &'static Path {
        Path::new(APP)
    }

    const APP: &str = "/Applications/Scriptorium.app/Contents/MacOS/scriptorium";

    fn parsed(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn empty_file_becomes_fresh_config() {
        for text in ["", "  \n"] {
            let out = merge(text, &Entry::new(APP)).unwrap();
            assert_eq!(parsed(&out), json!({"mcpServers": {"scriptorium": {"command": APP, "args": ["mcp"]}}}));
            assert!(out.ends_with('\n'));
            assert_eq!(state(&out, this()), State::Ours { command: APP.into() });
        }
        assert_eq!(state("", this()), State::Missing);
    }

    #[test]
    fn preferences_and_other_servers_kept_in_order() {
        let text = r#"{
  "preferences": {"zeta": true, "alpha": 1},
  "mcpServers": {
    "zz": {"command": "npx", "args": ["zz"]},
    "aa": {"command": "/bin/aa"}
  },
  "after": [1, 2]
}
"#;
        assert_eq!(state(text, this()), State::Missing);
        let out = merge(text, &Entry::new(APP)).unwrap();
        let v = parsed(&out);
        assert_eq!(v["preferences"], json!({"zeta": true, "alpha": 1}));
        assert_eq!(v["after"], json!([1, 2]));
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["preferences", "mcpServers", "after"]);
        let pref_keys: Vec<_> = v["preferences"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(pref_keys, ["zeta", "alpha"]);
        let servers: Vec<_> = v["mcpServers"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(servers, ["zz", "aa", "scriptorium"]);
    }

    #[test]
    fn ours_updated_in_place() {
        let text = r#"{"mcpServers": {
  "a": {"command": "a"},
  "scriptorium": {"command": "/Old/Scriptorium.app/Contents/MacOS/scriptorium", "args": ["old"], "env": {"X": "1"}},
  "b": {"command": "b"}
}}"#;
        assert_eq!(
            state(text, this()),
            State::Ours { command: "/Old/Scriptorium.app/Contents/MacOS/scriptorium".into() }
        );
        let out = merge(text, &Entry::new(APP)).unwrap();
        let v = parsed(&out);
        let servers: Vec<_> = v["mcpServers"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(servers, ["a", "scriptorium", "b"]);
        assert_eq!(v["mcpServers"]["scriptorium"], json!({"command": APP, "args": ["mcp"], "env": {"X": "1"}}));
        // No trailing newline in, none out.
        assert!(!out.ends_with('\n'));
        // Merging again changes nothing.
        assert_eq!(merge(&out, &Entry::new(APP)).unwrap(), out);
    }

    #[test]
    fn theirs_left_alone() {
        for text in [
            r#"{"mcpServers": {"scriptorium": {"command": "npx", "args": ["scriptorium-mcp"]}}}"#,
            r#"{"mcpServers": {"scriptorium": {"url": "https://example.com"}}}"#,
            r#"{"mcpServers": {"scriptorium": "weird"}}"#,
        ] {
            assert_eq!(state(text, this()), State::Theirs);
            assert_eq!(merge(text, &Entry::new(APP)), Err(Refused::Theirs));
            assert_eq!(remove(text, this()), Err(Refused::Theirs));
        }
    }

    #[test]
    fn broken_refused() {
        for text in ["{\"mcpServers\": {", "[1, 2]", "{\"mcpServers\": []}", "{\"a\": 1,}"] {
            assert!(matches!(state(text, this()), State::Broken(_)), "{text}");
            assert!(matches!(merge(text, &Entry::new(APP)), Err(Refused::Broken(_))));
            assert!(matches!(remove(text, this()), Err(Refused::Broken(_))));
        }
    }

    #[test]
    fn null_servers_treated_as_none() {
        let text = r#"{"mcpServers": null, "preferences": {}}"#;
        assert_eq!(state(text, this()), State::Missing);
        let out = merge(text, &Entry::new(APP)).unwrap();
        assert_eq!(state(&out, this()), State::Ours { command: APP.into() });
    }

    #[test]
    fn unicode_and_spaces_in_path() {
        let path = "/Users/zoë/My Apps/Scénario ✍/Scriptorium.app/Contents/MacOS/scriptorium";
        let out = merge("{}", &Entry::new(path)).unwrap();
        assert_eq!(state(&out, this()), State::Ours { command: path.into() });
        assert_eq!(parsed(&out)["mcpServers"]["scriptorium"]["command"], json!(path));
    }

    #[test]
    fn remove_ours_keeps_the_rest() {
        let text =
            merge(r#"{"preferences": {"p": 1}, "mcpServers": {"x": {"command": "x"}}}"#, &Entry::new(APP)).unwrap();
        let out = remove(&text, this()).unwrap();
        assert_eq!(state(&out, this()), State::Missing);
        assert_eq!(parsed(&out), json!({"preferences": {"p": 1}, "mcpServers": {"x": {"command": "x"}}}));
        // Missing: unchanged, byte for byte.
        assert_eq!(remove(&out, this()).unwrap(), out);
        assert_eq!(remove("", this()).unwrap(), "");
    }

    #[test]
    fn bom_accepted() {
        let out = merge("\u{feff}{\"a\": 1}", &Entry::new(APP)).unwrap();
        assert_eq!(parsed(&out)["a"], json!(1));
    }

    #[test]
    fn dev_path_is_ours_merged_twice_then_removed() {
        let dev = Path::new("/home/me/src/scriptorium/target/debug/scriptorium");
        let once = merge(r#"{"preferences": {}}"#, &Entry::new(dev)).unwrap();
        assert_eq!(state(&once, dev), State::Ours { command: dev.into() });
        // Another copy's view: a dev path isn't a Scriptorium.app, so theirs.
        assert_eq!(state(&once, this()), State::Theirs);
        let twice = merge(&once, &Entry::new(dev)).unwrap();
        assert_eq!(twice, once);
        assert_eq!(parsed(&remove(&twice, dev).unwrap()), json!({"preferences": {}, "mcpServers": {}}));
    }

    #[test]
    fn extra_fields_on_ours_kept() {
        let old = "/Old/Scriptorium.app/Contents/MacOS/scriptorium";
        let text = format!(
            r#"{{"mcpServers": {{"scriptorium": {{"disabled": true, "command": "{old}", "args": ["mcp"], "env": {{"A": "b"}}}}}}}}"#
        );
        let out = merge(&text, &Entry::new(APP)).unwrap();
        let v = parsed(&out);
        assert_eq!(
            v["mcpServers"]["scriptorium"],
            json!({"disabled": true, "command": APP, "args": ["mcp"], "env": {"A": "b"}})
        );
        let keys: Vec<_> = v["mcpServers"]["scriptorium"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["disabled", "command", "args", "env"]);
    }

    #[test]
    fn duplicate_keys_refused() {
        for text in [
            r#"{"a": 1, "a": 2}"#,
            r#"{"preferences": {"x": 1, "x": 1}}"#,
            r#"{"mcpServers": {"s": {"command": "a"}, "s": {"command": "b"}}}"#,
            r#"{"list": [{"k": 1, "k": 2}]}"#,
        ] {
            assert!(matches!(state(text, this()), State::Broken(ref why) if why.contains("twice")), "{text}");
            assert!(matches!(merge(text, &Entry::new(APP)), Err(Refused::Broken(_))));
            assert!(matches!(remove(text, this()), Err(Refused::Broken(_))));
        }
    }

    #[test]
    fn crlf_kept() {
        let text = "{\r\n  \"preferences\": {\r\n    \"a\": 1\r\n  }\r\n}\r\n";
        let out = merge(text, &Entry::new(APP)).unwrap();
        assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
        assert!(out.starts_with("{\r\n  \"preferences\": {\r\n    \"a\": 1\r\n  },\r\n  \"mcpServers\""));
        assert!(out.ends_with("}\r\n"));
        assert_eq!(
            remove(&out, this()).unwrap(),
            text.replace("\r\n  }\r\n}", "\r\n  },\r\n  \"mcpServers\": {}\r\n}")
        );
        // LF stays LF.
        assert!(!merge("{}\n", &Entry::new(APP)).unwrap().contains('\r'));
    }
}
