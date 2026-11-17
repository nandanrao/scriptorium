//! Claude Code: `~/.claude.json`, user scope (the top-level `mcpServers`,
//! as `claude mcp add --scope user` writes it). The file holds much else
//! (projects, settings, history); all of it is kept in order. Project
//! (`projects.<path>.mcpServers`) entries are not ours and not touched.

use std::path::Path;

use serde_json::{Map, Value};

use crate::{Entry, Refused, State, json};

/// What the config says about the user-scope `scriptorium` server.
pub fn state(text: &str, this: &Path) -> State {
    json::state(text, this)
}

/// The config with our entry added, or ours updated in place. A new entry
/// has the shape `claude mcp add` writes: `type`, `command`, `args`, `env`.
pub fn merge(text: &str, entry: &Entry) -> Result<String, Refused> {
    json::merge(text, entry, |command, args| {
        let mut m = Map::new();
        m.insert("type".to_owned(), Value::String("stdio".to_owned()));
        m.insert("command".to_owned(), Value::String(command.to_owned()));
        m.insert("args".to_owned(), Value::Array(args.iter().cloned().map(Value::String).collect()));
        m.insert("env".to_owned(), Value::Object(Map::new()));
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
        let out = merge("", &Entry::new(APP)).unwrap();
        assert_eq!(
            parsed(&out),
            json!({"mcpServers": {"scriptorium":
                {"type": "stdio", "command": APP, "args": ["mcp"], "env": {}}}})
        );
        assert_eq!(state(&out, this()), State::Ours { command: APP.into() });
    }

    #[test]
    fn other_keys_and_servers_kept_in_order() {
        let text = r#"{
  "numStartups": 12,
  "projects": {"/p": {"mcpServers": {"scriptorium": {"command": "npx"}}}},
  "mcpServers": {"github": {"type": "stdio", "command": "gh-mcp", "args": [], "env": {}}},
  "userID": "abc"
}
"#;
        // The project-scope entry is not the user-scope one.
        assert_eq!(state(text, this()), State::Missing);
        let out = merge(text, &Entry::new(APP)).unwrap();
        let v = parsed(&out);
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["numStartups", "projects", "mcpServers", "userID"]);
        assert_eq!(v["projects"], parsed(text)["projects"]);
        let servers: Vec<_> = v["mcpServers"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(servers, ["github", "scriptorium"]);
        assert!(out.ends_with("}\n"));
    }

    #[test]
    fn missing_servers_key_added_at_end() {
        let out = merge(r#"{"a": 1, "b": 2}"#, &Entry::new(APP)).unwrap();
        let keys: Vec<_> = parsed(&out).as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["a", "b", "mcpServers"]);
    }

    #[test]
    fn ours_updated_in_place() {
        let old = "/Volumes/Old/Scriptorium.app/Contents/MacOS/scriptorium";
        let text = format!(
            r#"{{"mcpServers": {{"scriptorium": {{"type": "stdio", "command": "{old}", "args": ["mcp"], "env": {{}}}}, "z": {{"command": "z"}}}}}}"#
        );
        assert_eq!(state(&text, this()), State::Ours { command: old.into() });
        let out = merge(&text, &Entry::new(APP)).unwrap();
        let v = parsed(&out);
        assert_eq!(
            v["mcpServers"]["scriptorium"],
            json!({"type": "stdio", "command": APP, "args": ["mcp"], "env": {}})
        );
        let servers: Vec<_> = v["mcpServers"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(servers, ["scriptorium", "z"]);
    }

    #[test]
    fn theirs_left_alone() {
        let text = r#"{"mcpServers": {"scriptorium": {"type": "stdio", "command": "/usr/local/bin/scriptorium", "args": ["mcp"]}}}"#;
        assert_eq!(state(text, this()), State::Theirs);
        assert_eq!(merge(text, &Entry::new(APP)), Err(Refused::Theirs));
        assert_eq!(remove(text, this()), Err(Refused::Theirs));
    }

    #[test]
    fn broken_refused() {
        let text = "{\"numStartups\": 3, \"mcpServers\": {\"x\":";
        assert!(matches!(state(text, this()), State::Broken(_)));
        assert!(matches!(merge(text, &Entry::new(APP)), Err(Refused::Broken(_))));
    }

    #[test]
    fn unicode_and_spaces_in_path() {
        let path = "/Users/Jürgen Ölberg/Apps (old)/Scriptorium.app/Contents/MacOS/scriptorium";
        let out = merge("{}", &Entry::new(path)).unwrap();
        assert_eq!(state(&out, this()), State::Ours { command: path.into() });
    }

    #[test]
    fn remove_ours() {
        let text = merge(r#"{"a": 1}"#, &Entry::new(APP)).unwrap();
        let out = remove(&text, this()).unwrap();
        assert_eq!(parsed(&out), json!({"a": 1, "mcpServers": {}}));
    }

    #[test]
    fn dev_path_is_ours_merged_twice_then_removed() {
        let dev = Path::new("/home/me/My Code/scriptorium/target/debug/scriptorium");
        let once = merge("", &Entry::new(dev)).unwrap();
        assert_eq!(state(&once, dev), State::Ours { command: dev.into() });
        assert_eq!(merge(&once, &Entry::new(dev)).unwrap(), once);
        let out = remove(&once, dev).unwrap();
        assert_eq!(state(&out, dev), State::Missing);
    }

    #[test]
    fn duplicate_keys_refused() {
        let text = r#"{"projects": {"/p": {"allowedTools": [], "allowedTools": []}}}"#;
        assert!(matches!(state(text, this()), State::Broken(_)));
        assert!(matches!(merge(text, &Entry::new(APP)), Err(Refused::Broken(_))));
    }

    /// `~/.claude.json` as Claude Code writes it (`JSON.stringify(_, null,
    /// 2)`, no final newline): only our entry differs after a merge.
    #[test]
    fn stringify_formatted_file_unchanged_but_for_our_entry() {
        let before = r#"{
  "numStartups": 42,
  "installMethod": "native",
  "autoUpdates": false,
  "tipsHistory": {
    "new-user-warmup": 7,
    "memory-command": 12
  },
  "promptQueueUseCount": 3,
  "userID": "0f3a",
  "projects": {
    "/Users/zoë/scripts": {
      "allowedTools": [],
      "history": [
        {
          "display": "fix \"the\" scene \u001f ✍",
          "pastedContents": {}
        }
      ],
      "mcpContextUris": [],
      "mcpServers": {},
      "hasTrustDialogAccepted": true,
      "lastCost": 0.5,
      "lastDuration": 12345
    }
  },
  "mcpServers": {
    "github": {
      "type": "stdio",
      "command": "gh-mcp",
      "args": [],
      "env": {}
    }
  },
  "oauthAccount": null
}"#;
        let ours = format!(
            r#",
    "scriptorium": {{
      "type": "stdio",
      "command": "{APP}",
      "args": [
        "mcp"
      ],
      "env": {{}}
    }}"#
        );
        let at = before.find("\n  },\n  \"oauthAccount\"").unwrap();
        let expected = format!("{}{ours}{}", &before[..at], &before[at..]);
        let out = merge(before, &Entry::new(APP)).unwrap();
        assert_eq!(out, expected);
        assert_eq!(remove(&out, this()).unwrap(), before);
    }
}
