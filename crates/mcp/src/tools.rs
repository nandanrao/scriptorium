//! The tools: each one's schema, and how it maps onto a `scriptorium` command.

use std::path::PathBuf;
use std::process::Command;

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

const SCRIPT: &str = "Path to the screenplay (.fdx). Relative paths are resolved from the folder the server runs in.";
const REVIEW: &str =
    "Which attached review to use, by name or file. Leave out when only one review is attached to the script.";

/// Default and largest page for `list_paragraphs`: a feature film has a few
/// thousand paragraphs, far more than is useful in one tool result.
const PAGE: u64 = 200;
const MAX_PAGE: u64 = 1000;

pub fn list() -> Vec<Value> {
    let script = json!({ "type": "string", "description": SCRIPT });
    let review = json!({ "type": "string", "description": REVIEW });
    let read_only = json!({ "readOnlyHint": true });
    let undoable = json!({ "readOnlyHint": false, "destructiveHint": false });
    vec![
        tool(
            "script_info",
            "Summary of a screenplay: paragraph, scene and word counts, element types, and the characters with the most dialogue.",
            json!({ "script": script }),
            &["script"],
            &read_only,
        ),
        tool(
            "list_paragraphs",
            "The script's paragraphs with their ids, element type (Scene Heading, Action, Character, Dialogue, ...), \
             scene and speaker. Filter it; results are paged. Ids are what edit_paragraph and suggest_change take.",
            json!({
                "script": script,
                "contains": { "type": "string", "description": "Only paragraphs whose text contains this (ignoring case)." },
                "scene": { "type": "string", "description": "Only paragraphs in scenes whose heading contains this (ignoring case)." },
                "speaker": { "type": "string", "description": "Only paragraphs spoken by this character (ignoring case)." },
                "kind": { "type": "string", "description": "Only this element type, e.g. \"Dialogue\"." },
                "offset": { "type": "integer", "minimum": 0, "description": "Skip this many matches (for paging)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_PAGE, "description": format!("At most this many (default {PAGE}).") },
            }),
            &["script"],
            &read_only,
        ),
        tool(
            "list_reviews",
            "The review files attached to the script, with how many suggestions are decided.",
            json!({ "script": script }),
            &["script"],
            &read_only,
        ),
        tool(
            "review_summary",
            "For each attached review: suggestion counts, decisions so far, and suggestions whose line wasn't found in the script.",
            json!({ "script": script }),
            &["script"],
            &read_only,
        ),
        tool(
            "attach_reviews",
            "Attach review files (SCRIPT REVIEW v1 text files) to the script so they show up in the app and in these tools.",
            json!({ "script": script, "files": { "type": "array", "items": { "type": "string" }, "minItems": 1 } }),
            &["script", "files"],
            &undoable,
        ),
        tool(
            "detach_reviews",
            "Detach review files from the script. Decisions already made stay in the script's text.",
            json!({ "script": script, "files": { "type": "array", "items": { "type": "string" }, "minItems": 1 } }),
            &["script", "files"],
            &undoable,
        ),
        tool(
            "list_suggestions",
            "A review's suggestions, numbered from 1 in script order, as JSON: status, the paragraph's current text, \
             the proposed options, and why. `outdated` means the line changed after the suggestion was written.",
            json!({
                "script": script,
                "review": review,
                "pending_only": { "type": "boolean", "description": "Only suggestions nobody has decided yet." },
            }),
            &["script"],
            &read_only,
        ),
        tool(
            "suggest_change",
            "Propose a change to one line by adding a suggestion to a review file (created if missing). The person \
             decides it in the app. Checks the line exists and that the change would be visible. Prefer this over \
             edit_paragraph. Attach a new review file with attach_reviews so it shows up in the app.",
            json!({
                "script": script,
                "review_file": { "type": "string", "description": "The review file to add to, e.g. \"dialogue-pass.review.txt\"." },
                "paragraph_id": { "type": "integer", "description": "The line to change (from list_paragraphs). Give this or `before`." },
                "before": { "type": "string", "description": "The line's current text, to find it by. Give this or `paragraph_id`." },
                "options": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "One or more replacement texts for the whole line." },
                "why": { "type": "string", "description": "A short reason shown to the person." },
            }),
            &["script", "review_file", "options"],
            &undoable,
        ),
        tool(
            "decide_suggestion",
            "Decide one suggestion, as the person would in the app: accept an option, keep the current line, \
             rewrite it with your own text, or undo the decision (undecide). Only when the person asked you to decide.",
            json!({
                "script": script,
                "review": review,
                "n": { "type": "integer", "minimum": 1, "description": "The suggestion's number from list_suggestions." },
                "decision": { "type": "string", "enum": ["accept", "keep", "rewrite", "undecide"] },
                "option": { "type": "integer", "minimum": 1, "description": "With accept: which option (default 1)." },
                "text": { "type": "string", "description": "With rewrite: the new text for the line." },
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
                "output": { "type": "string", "description": "Where to write, instead of the default." },
            }),
            &["script"],
            &json!({ "readOnlyHint": false, "destructiveHint": true }),
        ),
    ]
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
    let a = Args(args.as_object().cloned().unwrap_or_default());
    let script = || a.str("script");
    let review_flag = |cmd: &mut Vec<String>| {
        if let Some(r) = a.opt_str("review") {
            cmd.extend(["--review".into(), r]);
        }
    };
    let cmd: Vec<String> = match name {
        "script_info" => vec!["info".into(), script()?],
        "list_paragraphs" => return list_paragraphs(&a),
        "list_reviews" => vec!["reviews".into(), script()?],
        "review_summary" => vec!["review".into(), script()?],
        "attach_reviews" | "detach_reviews" => {
            let verb = if name == "attach_reviews" { "add" } else { "remove" };
            let files = a.strs("files")?;
            if files.is_empty() {
                return Err("give at least one file".to_string().into());
            }
            [vec!["reviews".into(), script()?, verb.into()], files].concat()
        }
        "list_suggestions" => {
            let mut c = vec!["suggestions".into(), script()?, "--json".into()];
            if a.bool("pending_only") {
                c.push("--pending".into());
            }
            review_flag(&mut c);
            c
        }
        "suggest_change" => {
            let mut c = vec!["suggest".into(), a.str("review_file")?, script()?];
            match (a.opt_int("paragraph_id"), a.opt_str("before")) {
                (Some(id), None) => c.extend(["--para".into(), id.to_string()]),
                (None, Some(b)) => c.extend(["--before".into(), b]),
                _ => return Err("give exactly one of paragraph_id or before".to_string().into()),
            }
            let options = a.strs("options")?;
            if options.is_empty() {
                return Err("give at least one option".to_string().into());
            }
            for o in options {
                c.extend(["--after".into(), o]);
            }
            if let Some(why) = a.opt_str("why") {
                c.extend(["--why".into(), why]);
            }
            c
        }
        "decide_suggestion" => {
            let mut c = vec!["decide".into(), script()?];
            review_flag(&mut c);
            c.push(a.int("n")?.to_string());
            match a.str("decision")?.as_str() {
                "accept" => {
                    c.push("accept".into());
                    if let Some(k) = a.opt_int("option") {
                        c.push(k.to_string());
                    }
                }
                "rewrite" => c.extend(["rewrite".into(), a.str("text")?]),
                d @ ("keep" | "undecide") => c.push(d.into()),
                d => return Err(format!("unknown decision {d:?}: accept, keep, rewrite or undecide").into()),
            }
            c
        }
        "edit_paragraph" => vec!["edit".into(), script()?, a.int("paragraph_id")?.to_string(), a.str("text")?],
        "undo" | "redo" => vec![name.into(), script()?],
        "write_script" => {
            let mut c = vec!["apply".into(), script()?];
            if let Some(o) = a.opt_str("output") {
                c.extend(["-o".into(), o]);
            }
            c
        }
        _ => return Err(Failure::UnknownTool),
    };
    run(&cmd)
}

fn list_paragraphs(a: &Args) -> Result<String, Failure> {
    let out = run(&["dump".into(), a.str("script")?, "--json".into()])?;
    let rows: Vec<Value> = serde_json::from_str(&out).map_err(|e| format!("unexpected output from dump: {e}"))?;
    let lower = |k: &str| a.opt_str(k).map(|s| s.to_lowercase());
    let (contains, scene, speaker, kind) = (lower("contains"), lower("scene"), lower("speaker"), lower("kind"));
    let field = |row: &Value, k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_lowercase();
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|r| contains.as_ref().is_none_or(|c| field(r, "text").contains(c.as_str())))
        .filter(|r| scene.as_ref().is_none_or(|s| field(r, "scene").contains(s.as_str())))
        .filter(|r| speaker.as_ref().is_none_or(|s| field(r, "speaker") == *s))
        .filter(|r| kind.as_ref().is_none_or(|k| field(r, "kind") == *k))
        .collect();
    let offset = a.opt_int("offset").unwrap_or(0).max(0) as usize;
    let limit = a.opt_int("limit").map_or(PAGE, |l| l.clamp(1, MAX_PAGE as i64) as u64) as usize;
    let page: Vec<&Value> = matches.iter().skip(offset).take(limit).copied().collect();
    let more = offset + page.len() < matches.len();
    Ok(json!({ "total": matches.len(), "offset": offset, "more": more, "paragraphs": page }).to_string())
}

/// Runs `scriptorium <args>`; stdout on success, stdout and stderr on failure.
fn run(args: &[String]) -> Result<String, Failure> {
    let bin = cli_binary();
    let out = Command::new(&bin)
        .args(args.iter())
        .output()
        .map_err(|e| format!("can't run {}: {e}. Set SCRIPTORIUM_BIN to the scriptorium binary.", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
    if out.status.success() {
        // Notes on stderr (e.g. an attached review that can't be read) matter to the model too.
        Ok(if stderr.is_empty() { stdout } else { format!("{stdout}\n\n{stderr}") })
    } else {
        Err(Failure::Tool([stdout, stderr].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")))
    }
}

/// `$SCRIPTORIUM_BIN`, else `scriptorium` next to this server (how the plugin
/// and the app ship them), else `scriptorium` on the PATH.
fn cli_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("SCRIPTORIUM_BIN") {
        return p.into();
    }
    let name = format!("scriptorium{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(&name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| name.into())
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
    fn opt_int(&self, k: &str) -> Option<i64> {
        self.0.get(k).and_then(Value::as_i64)
    }
    fn int(&self, k: &str) -> Result<i64, String> {
        self.opt_int(k).ok_or(format!("missing `{k}` (a whole number)"))
    }
    fn bool(&self, k: &str) -> bool {
        self.0.get(k).and_then(Value::as_bool).unwrap_or(false)
    }
    fn strs(&self, k: &str) -> Result<Vec<String>, String> {
        let list = self.0.get(k).and_then(Value::as_array).ok_or(format!("missing `{k}` (a list of strings)"))?;
        list.iter().map(|v| v.as_str().map(str::to_string).ok_or(format!("`{k}` must be strings"))).collect()
    }
}
