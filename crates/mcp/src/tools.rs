//! The tools: the shared catalogue from `scriptorium_tools` (run through
//! [`scriptorium_cli::tool`]), plus the tools only MCP has, which call the
//! CLI's typed commands in-process.
//!
//! Paths: `script` is taken as given (relative to the server's folder, which
//! in Cowork and Claude Desktop isn't the script's, so the schema asks for an
//! absolute path). Every other path (`review_file`, `review`, attach/detach
//! `files`, `output`) is relative to the script's folder: that is where the
//! model expects them, whatever folder the server was started in.
//!
//! A `review_file` in a folder that doesn't exist yet: the folder is created
//! when it is inside the script's folder (e.g. `reviews/dialogue.review.txt`);
//! anywhere else it is an error, so a typo never scatters folders around the
//! disk.

use std::path::{Component, Path, PathBuf};

use scriptorium_cli::commands::{self, Choice, SuggestionRef};
use scriptorium_fsio::opened::script_dir;
use scriptorium_tools::{HostField, ToolSpec};
use serde_json::{Map, Value, json};

pub enum Failure {
    UnknownTool,
    /// The command ran and failed, or the arguments were wrong: told to the model.
    Tool(String),
}

impl From<String> for Failure {
    fn from(s: String) -> Self {
        Failure::Tool(s)
    }
}

const SCRIPT: &str = "Absolute path to the screenplay (.fdx). A relative path is taken from the folder the server \
                      was started in, which usually isn't the script's.";
const REVIEW: &str = "Which attached review to use, by name or file (a path relative to the script's folder). Leave \
                      out when only one review is attached to the script.";
const REVIEW_FILE: &str = "The review file to add to, e.g. \"dialogue-pass.review.txt\": a path relative to the \
                           script's folder (or absolute), created and attached if missing. Always give one, named \
                           for the pass, and keep it for the whole pass.";
const FILES: &str = "Review files: paths relative to the script's folder, or absolute.";

/// The shared tools that only read.
const READ_ONLY: &[&str] =
    &["script_info", "list_paragraphs", "read_scene", "read_script", "list_reviews", "list_suggestions"];

/// The shared catalogue with MCP's fields: `script` on every tool,
/// `review_file` on the suggesting tools, `before` on `suggest_change`.
fn shared() -> Vec<ToolSpec> {
    let mut specs = scriptorium_tools::catalogue();
    for spec in &mut specs {
        if spec.name == "suggest_change" {
            scriptorium_tools::add_before(spec);
        }
        scriptorium_tools::add_host_fields(spec, &[HostField::Script, HostField::ReviewFile]);
        spec.schema["properties"]["script"]["description"] = json!(SCRIPT);
        if spec.schema["properties"].get("review_file").is_some() {
            spec.schema["properties"]["review_file"]["description"] = json!(REVIEW_FILE);
        }
    }
    specs
}

pub fn list() -> Vec<Value> {
    let script = json!({ "type": "string", "description": SCRIPT });
    let review = json!({ "type": "string", "description": REVIEW });
    let read_only = json!({ "readOnlyHint": true });
    let undoable = json!({ "readOnlyHint": false, "destructiveHint": false });
    let mut tools: Vec<Value> = shared()
        .into_iter()
        .map(|s| {
            let annotations = if READ_ONLY.contains(&s.name.as_str()) { &read_only } else { &undoable };
            json!({ "name": s.name, "description": s.description, "inputSchema": s.schema, "annotations": annotations })
        })
        .collect();
    tools.extend([
        tool(
            "attach_reviews",
            "Attach review files (SCRIPT REVIEW v1 text files) to the script so they show up in the app and in these \
             tools. suggest_change and suggest_changes attach their review_file themselves.",
            json!({ "script": script, "files": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": FILES } }),
            &["script", "files"],
            &undoable,
        ),
        tool(
            "detach_reviews",
            "Detach review files from the script. Decisions already made stay in the script's text.",
            json!({ "script": script, "files": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": FILES } }),
            &["script", "files"],
            &undoable,
        ),
        tool(
            "decide_suggestion",
            "Decide one suggestion, as the person would in the app: accept an option, keep the current lines, \
             rewrite them with your own (text for a line suggestion, lines for a block), or undo the decision \
             (undecide). Only when the person asked you to decide.",
            json!({
                "script": script,
                "review": review,
                "n": {
                    "type": ["integer", "string"],
                    "minimum": 1,
                    "description": "The suggestion's number `n` from list_suggestions, or \"e<entry>\" with its stable \
                                    `entry` (e.g. \"e3\"), which doesn't shift when suggestions are added.",
                },
                "decision": { "type": "string", "enum": ["accept", "keep", "rewrite", "undecide"] },
                "option": { "type": "integer", "minimum": 1, "description": "With accept: which option (default 1)." },
                "text": { "type": "string", "description": "With rewrite of a line suggestion: the new text for the line (a block takes `lines`)." },
                "lines": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "kind": { "type": "string", "enum": scriptorium_tools::KINDS }, "text": { "type": "string" } },
                        "required": ["kind", "text"],
                        "additionalProperties": false,
                    },
                    "description": "With rewrite of a block suggestion: its new lines, {kind, text} each \
                                    (kind: Scene Heading, Action, Character, Parenthetical, Dialogue, Transition, \
                                    Shot or General). an option `[]` cuts the lines (give `lines: []`).",
                },
            }),
            &["script", "n", "decision"],
            &undoable,
        ),
        tool(
            "edit_paragraph",
            "Replace one paragraph's whole text directly (no review). One paragraph, no newlines.",
            json!({
                "script": script,
                "paragraph_id": { "type": "integer" },
                "text": { "type": "string" },
            }),
            &["script", "paragraph_id", "text"],
            &undoable,
        ),
        tool(
            "undo",
            "Undo the last change to the script (an edit or a decision, made here or in the app).",
            json!({ "script": script }),
            &["script"],
            &undoable,
        ),
        tool("redo", "Redo the last undone change.", json!({ "script": script }), &["script"], &undoable),
        tool(
            "write_script",
            "Write the script with every decision and edit so far to a new .fdx (default: \"<name> (scriptorium).fdx\" \
             next to the original, replaced if it exists). The original is never overwritten.",
            json!({
                "script": script,
                "output": { "type": "string", "description": "Where to write, instead of the default: a path relative to the script's folder, or absolute." },
            }),
            &["script"],
            &json!({ "readOnlyHint": false, "destructiveHint": true }),
        ),
    ]);
    tools
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str], annotations: &Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": annotations,
    })
}

pub fn call(name: &str, args: &Value) -> Result<String, Failure> {
    let mut a = Args(args.as_object().cloned().unwrap_or_default());
    if let Some(script) = a.opt_str("script") {
        a.0.insert("script".into(), Value::String(expand_home(&script)));
    }
    let mut notes = vec![];
    // MCP-only tools first, through the CLI's typed commands.
    let result = match name {
        "attach_reviews" | "detach_reviews" => {
            let script = a.str("script")?;
            let files = a.strs("files")?;
            if files.is_empty() {
                return Err("give at least one file".to_string().into());
            }
            let files: Vec<String> = files.iter().map(|f| in_script_dir(&script, f)).collect::<Result<_, _>>()?;
            let files: Vec<&str> = files.iter().map(String::as_str).collect();
            if name == "attach_reviews" {
                commands::add_reviews(&script, &files, &mut notes)
            } else {
                commands::remove_reviews(&script, &files)
            }
        }
        "decide_suggestion" => {
            let script = a.str("script")?;
            // A review named by a file in the script's folder is that file;
            // otherwise it's an attached review's name.
            let review = match a.opt_str("review") {
                Some(r) => {
                    let at = in_script_dir(&script, &r)?;
                    Some(if Path::new(&at).is_file() { at } else { r })
                }
                None => None,
            };
            let n = a.suggestion_ref("n")?;
            let choice = match a.str("decision")?.as_str() {
                "accept" => Choice::Accept(a.opt_positive("option")?),
                "keep" => Choice::Keep,
                "rewrite" => match a.0.get("lines").filter(|v| !v.is_null()) {
                    Some(lines) => Choice::RewriteBlock(commands::block_lines(&typed_lines(lines)?)?),
                    None => Choice::Rewrite(a.str("text")?),
                },
                "undecide" => Choice::Undecide,
                d => return Err(format!("unknown decision {d:?}: accept, keep, rewrite or undecide").into()),
            };
            commands::decide(&script, review.as_deref(), n, choice, &mut notes)
        }
        "edit_paragraph" => {
            let id =
                a.0.get("paragraph_id")
                    .and_then(Value::as_u64)
                    .ok_or("missing `paragraph_id` (a paragraph id)".to_string())?;
            commands::edit(&a.str("script")?, id, &a.str("text")?, &[], &mut notes)
        }
        "undo" | "redo" => commands::undo_redo(&a.str("script")?, name == "redo", &[], &mut notes),
        "write_script" => {
            let script = a.str("script")?;
            let output = a.opt_str("output").map(|o| in_script_dir(&script, &o)).transpose()?;
            commands::apply(&script, &[], false, output.as_deref().map(Path::new), &mut notes)
        }
        _ if scriptorium_tools::catalogue().iter().any(|s| s.name == name) => {
            let mut rest = a.0.clone();
            rest.remove("script");
            if let Some(f) = rest.get("review_file").and_then(Value::as_str) {
                let at = review_file_at(&a.str("script")?, f)?;
                rest.insert("review_file".into(), Value::String(at));
            }
            return shared_tool(name, &a, Value::Object(rest));
        }
        _ => return Err(Failure::UnknownTool),
    };
    finish(result, &notes)
}

/// A leading `~` or `~/` as `$HOME` (models write `~/Scripts/x.fdx`, and no
/// shell is involved to expand it). Anything else, including `~name`, as given.
fn expand_home(path: &str) -> String {
    expand_home_with(path, std::env::var("HOME").ok().as_deref())
}

fn expand_home_with(path: &str, home: Option<&str>) -> String {
    match (home, path.strip_prefix('~')) {
        (Some(h), Some("")) if !h.is_empty() => h.to_string(),
        (Some(h), Some(rest)) if !h.is_empty() && rest.starts_with('/') => format!("{}{rest}", h.trim_end_matches('/')),
        _ => path.to_string(),
    }
}

/// `path` (from a tool argument) as a path from the server's folder: as
/// given when absolute, else joined to the script's folder.
fn in_script_dir(script: &str, path: &str) -> Result<String, String> {
    let path = &expand_home(path);
    if Path::new(path).is_absolute() {
        return Ok(path.to_string());
    }
    Ok(script_dir(script)?.join(path).to_string_lossy().into_owned())
}

/// A `review_file` resolved like [`in_script_dir`], creating its folder if
/// it's missing and inside the script's folder (see the module docs).
fn review_file_at(script: &str, file: &str) -> Result<String, String> {
    let at = in_script_dir(script, file)?;
    let parent = match Path::new(&at).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => return Ok(at),
    };
    if parent.is_dir() {
        return Ok(at);
    }
    let dir = script_dir(script)?;
    if normalize(&parent).starts_with(&dir) {
        std::fs::create_dir_all(&parent).map_err(|e| format!("can't create the folder {}: {e}", parent.display()))?;
        Ok(at)
    } else {
        Err(format!(
            "the folder {} doesn't exist; put review_file in the script's folder (or a folder inside it), or in a \
             folder that exists",
            parent.display()
        ))
    }
}

/// `p` with `.` and `..` taken out, without touching the disk (the folder
/// doesn't exist yet, so it can't be canonicalized).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Runs a shared tool on `a`'s script with `args` (without `script`, and with
/// `review_file` already resolved against the script's folder).
fn shared_tool(name: &str, a: &Args, args: Value) -> Result<String, Failure> {
    let script = a.str("script")?;
    let mut notes = vec![];
    let result = scriptorium_cli::tool(&script, name, &args, None, &mut notes);
    finish(result, &notes)
}

/// The tool result: the output, or the error, with any notes (e.g. an
/// attached review that can't be read) after it; they matter to the model too.
fn finish(result: Result<String, String>, notes: &[String]) -> Result<String, Failure> {
    let notes = notes.iter().map(|n| format!("note: {n}")).collect::<Vec<_>>().join("\n");
    let join = |text: String| {
        let text = text.trim_end().to_string();
        if notes.is_empty() { text } else { format!("{text}\n\n{notes}") }
    };
    result.map(join).map_err(|e| Failure::Tool(join(e)))
}

/// `lines` (`[{kind, text}]`) as `TYPE: text` lines for
/// [`commands::block_lines`]; `[]` is a cut.
fn typed_lines(lines: &Value) -> Result<String, String> {
    let list = lines.as_array().ok_or("`lines` must be a list of {kind, text}")?;
    if list.is_empty() {
        return Ok("[cut]".into());
    }
    let mut out = String::new();
    for l in list {
        let (Some(kind), Some(text)) = (l.get("kind").and_then(Value::as_str), l.get("text").and_then(Value::as_str))
        else {
            return Err("each line is {\"kind\": <element type>, \"text\": <text>}".into());
        };
        if [kind, text].iter().any(|s| s.contains(['\n', '\r', '\u{2028}', '\u{2029}'])) {
            return Err("a line can't contain a newline: give each paragraph as its own line".into());
        }
        if kind.contains(':') {
            return Err(format!("unknown element type {kind:?}"));
        }
        out.push_str(&format!("{kind}: {text}\n"));
    }
    Ok(out)
}

/// A tool call's arguments, with errors phrased for the model.
struct Args(Map<String, Value>);

impl Args {
    fn opt_str(&self, k: &str) -> Option<String> {
        self.0.get(k).and_then(Value::as_str).map(str::to_string)
    }
    fn str(&self, k: &str) -> Result<String, String> {
        self.opt_str(k).ok_or(format!("missing `{k}` (a string)"))
    }
    /// A whole number from 1, if given.
    fn opt_positive(&self, k: &str) -> Result<Option<usize>, String> {
        match self.0.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .filter(|n| *n > 0)
                .and_then(|n| usize::try_from(n).ok())
                .map(Some)
                .ok_or(format!("`{k}` must be a whole number from 1")),
        }
    }
    /// A suggestion number (from 1), or `e<entry>` for its stable place in
    /// the file. Nothing else: not `-1`, `2.5`, `"3"` or `"x"`.
    fn suggestion_ref(&self, k: &str) -> Result<SuggestionRef, String> {
        let bad = || format!("`{k}` must be a suggestion number from 1, or \"e<entry>\" (e.g. \"e3\")");
        match self.0.get(k) {
            Some(Value::Number(n)) => match n.as_u64().filter(|n| *n > 0).and_then(|n| usize::try_from(n).ok()) {
                Some(n) => Ok(SuggestionRef::Number(n)),
                None => Err(bad()),
            },
            Some(Value::String(s)) => match SuggestionRef::parse(s) {
                Some(r @ SuggestionRef::Entry(_)) => Ok(r),
                _ => Err(bad()),
            },
            _ => Err(bad()),
        }
    }
    fn strs(&self, k: &str) -> Result<Vec<String>, String> {
        let list = self.0.get(k).and_then(Value::as_array).ok_or(format!("missing `{k}` (a list of strings)"))?;
        list.iter().map(|v| v.as_str().map(str::to_string).ok_or(format!("`{k}` must be strings"))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_tilde_is_the_home_folder() {
        assert_eq!(expand_home_with("~/Scripts/a.fdx", Some("/Users/w")), "/Users/w/Scripts/a.fdx");
        assert_eq!(expand_home_with("~", Some("/Users/w/")), "/Users/w/");
        assert_eq!(expand_home_with("~bob/a.fdx", Some("/Users/w")), "~bob/a.fdx");
        assert_eq!(expand_home_with("a/~/b", Some("/Users/w")), "a/~/b");
        assert_eq!(expand_home_with("~/a", None), "~/a");
        assert_eq!(in_script_dir("/x/film.fdx", "~/r.review.txt").map(|p| p.starts_with('~')), Ok(false));
    }

    #[test]
    fn names_are_unique_and_every_tool_takes_script() {
        let tools = list();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for t in &tools {
            let schema = &t["inputSchema"];
            assert_eq!(schema["properties"]["script"]["description"], SCRIPT, "{}", t["name"]);
            assert!(schema["required"].as_array().unwrap().contains(&json!("script")), "{}", t["name"]);
        }
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn instructions_name_only_real_tools() {
        let specs: Vec<ToolSpec> = list()
            .into_iter()
            .map(|t| ToolSpec {
                name: t["name"].as_str().unwrap().into(),
                description: String::new(),
                schema: Value::Null,
            })
            .collect();
        // Snake-case words that aren't tools must be parameters some tool takes.
        let unknown = scriptorium_tools::prompt::unknown_tools(crate::INSTRUCTIONS, &specs);
        let params: Vec<String> = list()
            .iter()
            .flat_map(|t| t["inputSchema"]["properties"].as_object().unwrap().keys().cloned().collect::<Vec<_>>())
            .collect();
        let neither: Vec<&String> = unknown.iter().filter(|w| !params.contains(w)).collect();
        assert!(neither.is_empty(), "{neither:?}");
    }
}
