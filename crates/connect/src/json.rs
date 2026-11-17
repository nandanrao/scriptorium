//! The JSON configs (Claude Desktop, Claude Code): a top-level object with
//! an `mcpServers` object of named servers. The file is re-serialized
//! whole: key order is kept (serde_json's `preserve_order`, on for the
//! workspace), pretty-printed with two spaces as both agents (and
//! `JSON.stringify(_, null, 2)`) write it, with the input's line endings
//! (LF or CRLF) and final newline (or none). A duplicate key anywhere is
//! refused, since re-serializing would silently drop one of them.

use std::fmt;
use std::path::Path;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::{Entry, NAME, Refused, State, classify, command_str};

const SERVERS: &str = "mcpServers";

/// A JSON value that refuses duplicate object keys.
struct Strict(Value);

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Strict, E> {
        Ok(Strict(Value::Bool(v)))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Strict, E> {
        Ok(Strict(Value::from(v)))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Strict, E> {
        Ok(Strict(Value::from(v)))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Strict, E> {
        Ok(Strict(Value::from(v)))
    }
    fn visit_str<E>(self, v: &str) -> Result<Strict, E> {
        Ok(Strict(Value::String(v.to_owned())))
    }
    fn visit_string<E>(self, v: String) -> Result<Strict, E> {
        Ok(Strict(Value::String(v)))
    }
    fn visit_unit<E>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }
    fn visit_none<E>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(v)) = seq.next_element()? {
            items.push(v);
        }
        Ok(Strict(Value::Array(items)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
        let mut out = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if out.contains_key(&key) {
                return Err(de::Error::custom(format_args!("the key \"{key}\" appears twice")));
            }
            let Strict(v) = map.next_value()?;
            out.insert(key, v);
        }
        Ok(Strict(Value::Object(out)))
    }
}

/// A parsed config: `None` for an empty file.
fn parse(text: &str) -> Result<Option<Map<String, Value>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Strict>(text) {
        Ok(Strict(Value::Object(map))) => Ok(Some(map)),
        Ok(_) => Err("the file isn't a JSON object".to_owned()),
        Err(e) => Err(format!("not valid JSON ({e})")),
    }
}

fn servers(root: &Map<String, Value>) -> Result<Option<&Map<String, Value>>, String> {
    match root.get(SERVERS) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m)),
        Some(_) => Err(format!("\"{SERVERS}\" isn't an object")),
    }
}

fn entry_state(server: &Value, this: &Path) -> State {
    classify(server.get("command").and_then(Value::as_str), this)
}

pub(crate) fn state(text: &str, this: &Path) -> State {
    let root = match parse(text) {
        Ok(Some(root)) => root,
        Ok(None) => return State::Missing,
        Err(why) => return State::Broken(why),
    };
    match servers(&root) {
        Err(why) => State::Broken(why),
        Ok(None) => State::Missing,
        Ok(Some(servers)) => servers.get(NAME).map_or(State::Missing, |s| entry_state(s, this)),
    }
}

fn render(root: Map<String, Value>, original: &str) -> String {
    let mut out = serde_json::to_string_pretty(&Value::Object(root)).expect("a JSON value always serializes");
    if original.trim().is_empty() || original.ends_with('\n') {
        out.push('\n');
    }
    // JSON strings hold no raw newlines, so every one is a line break.
    if original.contains("\r\n") { out.replace('\n', "\r\n") } else { out }
}

/// Add our entry or update ours in place. `fresh` is the full entry for a
/// new server; updating ours sets only `command` and `args`, keeping any
/// other fields (an `env` the user added) and the entry's position.
pub(crate) fn merge(
    text: &str,
    entry: &Entry,
    fresh: impl FnOnce(&str, &[String]) -> Map<String, Value>,
) -> Result<String, Refused> {
    let command = command_str(entry)?;
    let mut root = parse(text).map_err(Refused::Broken)?.unwrap_or_default();
    let existing = servers(&root).map_err(Refused::Broken)?;
    let current = existing.and_then(|s| s.get(NAME)).map(|s| entry_state(s, &entry.command));
    if current == Some(State::Theirs) {
        return Err(Refused::Theirs);
    }
    let servers = root.entry(SERVERS).or_insert_with(|| Value::Object(Map::new()));
    if servers.is_null() {
        *servers = Value::Object(Map::new());
    }
    let servers = servers.as_object_mut().expect("checked above");
    let args = Value::Array(entry.args.iter().cloned().map(Value::String).collect());
    match servers.get_mut(NAME).and_then(Value::as_object_mut) {
        Some(ours) => {
            ours.insert("command".to_owned(), Value::String(command.to_owned()));
            ours.insert("args".to_owned(), args);
        }
        None => {
            servers.insert(NAME.to_owned(), Value::Object(fresh(command, &entry.args)));
        }
    }
    Ok(render(root, text))
}

/// Take our entry out. A file without one comes back unchanged.
pub(crate) fn remove(text: &str, this: &Path) -> Result<String, Refused> {
    match state(text, this) {
        State::Missing => return Ok(text.to_owned()),
        State::Theirs => return Err(Refused::Theirs),
        State::Broken(why) => return Err(Refused::Broken(why)),
        State::Ours { .. } => {}
    }
    let mut root = parse(text).map_err(Refused::Broken)?.expect("ours is in a non-empty file");
    if let Some(Value::Object(servers)) = root.get_mut(SERVERS) {
        servers.shift_remove(NAME);
    }
    Ok(render(root, text))
}
