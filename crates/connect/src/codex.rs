//! Codex: `~/.codex/config.toml`, a table per server:
//! `[mcp_servers.scriptorium]` with `command`, `args` and
//! `tool_timeout_sec`. Edited with `toml_edit`, so the user's comments,
//! order and layout stay.

use std::path::Path;

use toml_edit::{Array, DocumentMut, Item, Table, TableLike, Value};

use crate::{Entry, NAME, Refused, State, classify, command_str};

const SERVERS: &str = "mcp_servers";

/// Long translation batches; Codex's default is 60.
pub const TOOL_TIMEOUT_SEC: i64 = 300;

fn parse(text: &str) -> Result<DocumentMut, String> {
    text.parse::<DocumentMut>().map_err(|e| format!("not valid TOML ({})", e.to_string().trim_end()))
}

fn servers(doc: &DocumentMut) -> Result<Option<&dyn TableLike>, String> {
    match doc.get(SERVERS) {
        None => Ok(None),
        Some(item) => item.as_table_like().map(Some).ok_or_else(|| format!("\"{SERVERS}\" isn't a table")),
    }
}

fn entry_state(item: &Item, this: &Path) -> State {
    classify(item.as_table_like().and_then(|t| t.get("command")).and_then(Item::as_str), this)
}

/// What the config says about the `scriptorium` server.
pub fn state(text: &str, this: &Path) -> State {
    let doc = match parse(text) {
        Ok(doc) => doc,
        Err(why) => return State::Broken(why),
    };
    match servers(&doc) {
        Err(why) => State::Broken(why),
        Ok(None) => State::Missing,
        Ok(Some(servers)) => servers.get(NAME).map_or(State::Missing, |s| entry_state(s, this)),
    }
}

fn args_value(args: &[String]) -> Value {
    Value::Array(args.iter().map(String::as_str).collect::<Array>())
}

/// Set `key` to `value`, keeping the old value's comments and spacing.
fn set(table: &mut dyn TableLike, key: &str, mut value: Value) {
    if let Some(old) = table.get_mut(key).and_then(Item::as_value_mut) {
        *value.decor_mut() = old.decor().clone();
        *old = value;
    } else {
        table.insert(key, Item::Value(value));
    }
}

/// The config with our entry added, or ours updated in place. Updating
/// ours sets `command` and `args`, adds `tool_timeout_sec` only if it is
/// missing (a value the user chose stays), and keeps any other fields.
pub fn merge(text: &str, entry: &Entry) -> Result<String, Refused> {
    let command = command_str(entry)?;
    let mut doc = parse(text).map_err(Refused::Broken)?;
    let current =
        servers(&doc).map_err(Refused::Broken)?.and_then(|s| s.get(NAME)).map(|s| entry_state(s, &entry.command));
    if current == Some(State::Theirs) {
        return Err(Refused::Theirs);
    }
    if !doc.contains_key(SERVERS) {
        let mut t = Table::new();
        t.set_implicit(true);
        doc.insert(SERVERS, Item::Table(t));
    }
    let servers = doc[SERVERS].as_table_like_mut().expect("checked above");
    match servers.get_mut(NAME).and_then(Item::as_table_like_mut) {
        Some(ours) => {
            set(ours, "command", command.into());
            set(ours, "args", args_value(&entry.args));
            if !ours.contains_key("tool_timeout_sec") {
                ours.insert("tool_timeout_sec", toml_edit::value(TOOL_TIMEOUT_SEC));
            }
        }
        None => {
            let mut t = Table::new();
            t.insert("command", toml_edit::value(command));
            t.insert("args", Item::Value(args_value(&entry.args)));
            t.insert("tool_timeout_sec", toml_edit::value(TOOL_TIMEOUT_SEC));
            servers.insert(NAME, Item::Table(t));
        }
    }
    Ok(doc.to_string())
}

/// The config without our entry (unchanged if it has none).
pub fn remove(text: &str, this: &Path) -> Result<String, Refused> {
    match state(text, this) {
        State::Missing => return Ok(text.to_owned()),
        State::Theirs => return Err(Refused::Theirs),
        State::Broken(why) => return Err(Refused::Broken(why)),
        State::Ours { .. } => {}
    }
    let mut doc = parse(text).map_err(Refused::Broken)?;
    if let Some(servers) = doc.get_mut(SERVERS).and_then(Item::as_table_like_mut) {
        servers.remove(NAME);
    }
    let out = doc.to_string();
    // Our table was first: the next one's blank line before it now leads.
    let blank_start = |t: &str| t.starts_with('\n') || t.starts_with("\r\n");
    if blank_start(&out) && !blank_start(text) {
        return Ok(out.trim_start_matches(['\r', '\n']).to_owned());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn this() -> &'static Path {
        Path::new(APP)
    }

    const APP: &str = "/Applications/Scriptorium.app/Contents/MacOS/scriptorium";

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    #[test]
    fn empty_file_becomes_fresh_config() {
        assert_eq!(state("", this()), State::Missing);
        let out = merge("", &Entry::new(APP)).unwrap();
        assert_eq!(
            out,
            format!("[mcp_servers.scriptorium]\ncommand = \"{APP}\"\nargs = [\"mcp\"]\ntool_timeout_sec = 300\n")
        );
        assert_eq!(state(&out, this()), State::Ours { command: APP.into() });
    }

    #[test]
    fn comments_order_and_other_servers_kept() {
        let text = r#"# my codex config
model = "o3"  # the model
approval_policy = "on-request"

[mcp_servers.github]   # GitHub
command = "gh-mcp"
args = []

# notes about tools
[tools]
web_search = true
"#;
        assert_eq!(state(text, this()), State::Missing);
        let out = merge(text, &Entry::new(APP)).unwrap();
        // Our table goes beside its sibling; everything else is unchanged.
        assert!(out.contains("args = []\n\n[mcp_servers.scriptorium]\n"), "{out}");
        assert_eq!(remove(&out, this()).unwrap(), text);
        let d = doc(&out);
        assert_eq!(d["mcp_servers"]["scriptorium"]["command"].as_str(), Some(APP));
        assert_eq!(d["mcp_servers"]["scriptorium"]["tool_timeout_sec"].as_integer(), Some(300));
        assert_eq!(d["mcp_servers"]["github"]["command"].as_str(), Some("gh-mcp"));
        assert_eq!(state(&out, this()), State::Ours { command: APP.into() });
    }

    #[test]
    fn ours_updated_in_place() {
        let text = r#"[mcp_servers.a]
command = "a"

# ours
[mcp_servers.scriptorium]
command = "/Old Place/Scriptorium.app/Contents/MacOS/scriptorium"  # moved?
args = ["old"]
tool_timeout_sec = 600
env = { X = "1" }

[mcp_servers.b]
command = "b"
"#;
        assert_eq!(
            state(text, this()),
            State::Ours { command: "/Old Place/Scriptorium.app/Contents/MacOS/scriptorium".into() }
        );
        let out = merge(text, &Entry::new(APP)).unwrap();
        let expected = text
            .replace("/Old Place/Scriptorium.app/Contents/MacOS/scriptorium", APP)
            .replace(r#"args = ["old"]"#, r#"args = ["mcp"]"#);
        assert_eq!(out, expected);
        assert_eq!(merge(&out, &Entry::new(APP)).unwrap(), out);
    }

    #[test]
    fn ours_without_timeout_gets_one() {
        let text = format!("[mcp_servers.scriptorium]\ncommand = \"{APP}\"\nargs = [\"mcp\"]\n");
        let out = merge(&text, &Entry::new(APP)).unwrap();
        assert_eq!(doc(&out)["mcp_servers"]["scriptorium"]["tool_timeout_sec"].as_integer(), Some(300));
    }

    #[test]
    fn inline_and_dotted_forms() {
        let inline = "mcp_servers = { other = { command = \"o\" } }\n";
        let out = merge(inline, &Entry::new(APP)).unwrap();
        assert_eq!(state(&out, this()), State::Ours { command: APP.into() });
        assert_eq!(doc(&out)["mcp_servers"]["other"]["command"].as_str(), Some("o"));

        let dotted = format!("mcp_servers.scriptorium.command = \"{APP}\"\n");
        assert_eq!(state(&dotted, this()), State::Ours { command: APP.into() });
        let out = merge(&dotted, &Entry::new(APP)).unwrap();
        assert_eq!(state(&out, this()), State::Ours { command: APP.into() });
        assert_eq!(doc(&out)["mcp_servers"]["scriptorium"]["args"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn theirs_left_alone() {
        for text in [
            "[mcp_servers.scriptorium]\ncommand = \"/opt/homebrew/bin/scriptorium\"\n",
            "[mcp_servers.scriptorium]\nurl = \"https://example.com/mcp\"\n",
            "mcp_servers.scriptorium = 3\n",
        ] {
            assert_eq!(state(text, this()), State::Theirs, "{text}");
            assert_eq!(merge(text, &Entry::new(APP)), Err(Refused::Theirs));
            assert_eq!(remove(text, this()), Err(Refused::Theirs));
        }
    }

    #[test]
    fn broken_refused() {
        for text in ["[mcp_servers\ncommand = 1\n", "mcp_servers = 3\n", "a = 1\na = 2\n"] {
            assert!(matches!(state(text, this()), State::Broken(_)), "{text}");
            assert!(matches!(merge(text, &Entry::new(APP)), Err(Refused::Broken(_))));
            assert!(matches!(remove(text, this()), Err(Refused::Broken(_))));
        }
    }

    #[test]
    fn unicode_and_spaces_in_path() {
        let path = "/Users/zoë/My Apps/Scénario \"✍\"/Scriptorium.app/Contents/MacOS/scriptorium";
        let out = merge("# hi\n", &Entry::new(path)).unwrap();
        assert!(out.contains("# hi\n"), "{out}");
        assert_eq!(state(&out, this()), State::Ours { command: path.into() });
    }

    #[test]
    fn remove_ours_keeps_the_rest() {
        let text = "# top\n[mcp_servers.x]\ncommand = \"x\" # keep\n";
        let with = merge(text, &Entry::new(APP)).unwrap();
        let out = remove(&with, this()).unwrap();
        assert_eq!(out, text);
        assert_eq!(remove(&out, this()).unwrap(), out);
        assert_eq!(remove("", this()).unwrap(), "");
    }

    #[test]
    fn dev_path_is_ours_merged_twice_then_removed() {
        let dev = Path::new("/home/me/src/scriptorium/target/debug/scriptorium");
        let text = "model = \"o3\"\n";
        let once = merge(text, &Entry::new(dev)).unwrap();
        assert_eq!(state(&once, dev), State::Ours { command: dev.into() });
        assert_eq!(state(&once, this()), State::Theirs);
        assert_eq!(merge(&once, &Entry::new(dev)).unwrap(), once);
        assert_eq!(remove(&once, dev).unwrap(), text);
    }

    #[test]
    fn remove_ours_first_leaves_no_leading_blank_line() {
        let text = "[mcp_servers.x]\ncommand = \"x\"\n";
        let ours_first = format!("[mcp_servers.scriptorium]\ncommand = \"{APP}\"\nargs = [\"mcp\"]\n\n{text}");
        assert_eq!(remove(&ours_first, this()).unwrap(), text);
    }

    #[test]
    fn env_subtables() {
        let text = "[mcp_servers.x]\ncommand = \"x\"\n\n[mcp_servers.x.env]\nA = \"1\"\n";
        let out = merge(text, &Entry::new(APP)).unwrap();
        // The new table comes after x and its env.
        assert!(out.starts_with(text), "{out}");
        assert!(out.contains("[mcp_servers.scriptorium]"));
        assert_eq!(remove(&out, this()).unwrap(), text);

        let old = "/Old/Scriptorium.app/Contents/MacOS/scriptorium";
        let ours = format!(
            "[mcp_servers.scriptorium]\ncommand = \"{old}\"\nargs = [\"mcp\"]\n\n[mcp_servers.scriptorium.env]\nB = \"2\"\n\n{text}"
        );
        let out = merge(&ours, &Entry::new(APP)).unwrap();
        assert_eq!(doc(&out)["mcp_servers"]["scriptorium"]["env"]["B"].as_str(), Some("2"));
        assert!(out.contains("[mcp_servers.scriptorium.env]\nB = \"2\"\n"), "{out}");
        assert_eq!(remove(&out, this()).unwrap(), text);
    }
}
