//! Names, descriptions and argument schemas, per [`Offer`]: the chat's
//! catalogue, and the agents' (the chat's with `script`, `review_file` and
//! `before`, plus the agent-only tools in [`crate::agent`]).

pub use scriptorium_agent::ToolSpec;
use serde_json::{Value, json};

use crate::ToolError;
use crate::block::MAX_LINES;
use crate::read::{MIN_READ_BUDGET, READ_BUDGET};
use crate::rows::{MAX_ROWS_PAGE, ROWS_PAGE};

/// The element types a suggested line can have (any case is accepted).
pub const KINDS: [&str; 8] =
    ["Scene Heading", "Action", "Character", "Parenthetical", "Dialogue", "Transition", "Shot", "General"];

/// Default and largest page for `list_paragraphs`: a feature film has a few
/// thousand paragraphs, far more than is useful in one result.
pub const PAGE: u64 = 200;
pub const MAX_PAGE: u64 = 1000;
/// Most suggestions in one `suggest_changes` call.
pub const MAX_BATCH: usize = 200;
/// Most options in one suggestion.
pub const MAX_OPTIONS: usize = 3;

/// Which tools a host offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// The built-in agent (the in-app chat and `scriptorium chat`), bound to
    /// its script and its thread's review. Threads store a hash of this
    /// catalogue (`tools_hash`), so any change to it shows the "older tools"
    /// notice.
    Chat,
    /// Outside agents (MCP, the CLI `tool`): the chat's tools with `script`
    /// on each, `review_file` on the suggesting tools and `before` on
    /// `suggest_change`, then the agent-only tools
    /// ([`AGENT_ONLY`](crate::agent::AGENT_ONLY)).
    Agent,
}

/// The tools `offer` gives a model, in the order they are listed.
pub fn catalogue(offer: Offer) -> Vec<ToolSpec> {
    match offer {
        Offer::Chat => chat(),
        Offer::Agent => crate::agent::offer(chat()),
    }
}

/// `name`'s spec as `offer` gives it, or [`ToolError::Unknown`] if that
/// offer has no such tool (an agent-only tool under [`Offer::Chat`]). With
/// [`check_args`], what a host checks a call against.
pub fn offered(offer: Offer, name: &str) -> Result<ToolSpec, ToolError> {
    catalogue(offer).into_iter().find(|s| s.name == name).ok_or_else(|| ToolError::Unknown(name.to_string()))
}

fn chat() -> Vec<ToolSpec> {
    let line = json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": KINDS, "description": "The element type." },
            "text": { "type": "string", "description": "The paragraph's text. No newlines." },
        },
        "required": ["kind", "text"],
        "additionalProperties": false,
    });
    let options = json!({
        "type": "array", "minItems": 1, "maxItems": MAX_OPTIONS,
        "items": { "anyOf": [
            { "type": "string" },
            { "type": "array", "maxItems": MAX_LINES, "items": line },
        ] },
        "description": format!(
            "One to three options, best first: one when the line is obvious, two or three only for a real choice \
             (meaning, formality, an idiom, a pun, length), with a short why saying how they differ. With paragraph_id, each option can be a string: the line's new \
             text, no newlines. Otherwise each option is a list of lines [{{kind, text}}] that replaces every \
             target line (at most {MAX_LINES}); an option `[]` cuts the lines. With insert_after, each \
             option is just the new lines."
        ),
    });
    let why = json!({ "type": "string", "description": "A short reason the writer will see." });
    let paragraph_id = json!({ "type": "integer", "description": "One line to change: the number after ¶ in read_scene, or an id from list_paragraphs." });
    let paragraph_ids = json!({
        "type": "array", "items": { "type": "integer" }, "minItems": 1, "maxItems": MAX_LINES,
        "description": "Lines that only make sense together, changed as one suggestion the writer takes whole: \
                        every id of the lines, consecutive and in script order. read_scene leaves empty lines \
                        out: ids between two listed ones that you didn't see are refused, and the error lists them \
                        so you can include them.",
    });
    let insert_after = json!({ "type": "integer", "description": "Add lines after this one (it stays as it is); the options are the new lines." });
    let row = json!({
        "type": "integer", "minimum": 1,
        "description": "On a version: the row this suggestion is for, as read_rows numbers it (optional: the row is \
                        found from the lines). A suggestion on a version names one row's lines on this side (its \
                        `this` ids), all of them: paragraph_id, or paragraph_ids when there are several, never \
                        insert_after; to change the number of lines, give each option as a list of {kind, text} \
                        lines. A row not yet translated takes a suggestion on its blank lines.",
    });
    let target = json!({ "paragraph_id": paragraph_id, "paragraph_ids": paragraph_ids, "insert_after": insert_after, "row": row, "options": options, "why": why });
    vec![
        spec(
            "script_info",
            "Summary of the script: paragraph, scene and word counts, element types, the characters with the most \
             dialogue, and how many reviews are attached.",
            json!({}),
            &[],
        ),
        spec(
            "list_paragraphs",
            "The script's paragraphs with their ids, element type (Scene Heading, Action, Character, Dialogue, ...), \
             scene and speaker, as JSON. Filter it; results are paged. Ids are what suggest_change takes.",
            json!({
                "contains": { "type": "string", "description": "Only paragraphs whose text contains this (ignoring case)." },
                "scene": { "type": "string", "description": "Only paragraphs in scenes whose heading contains this (ignoring case)." },
                "speaker": { "type": "string", "description": "Only paragraphs spoken by this character (ignoring case)." },
                "kind": { "type": "string", "description": "Only this element type, e.g. \"Dialogue\"." },
                "offset": { "type": "integer", "minimum": 0, "description": "Skip this many matches (for paging)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_PAGE, "description": format!("At most this many (default {PAGE}).") },
            }),
            &[],
        ),
        spec(
            "read_scene",
            "One scene as the script reads now, one paragraph per line: `¶<paragraph_id> <text>`. paragraph_id is \
             the number after ¶. Elements the script shows in capitals (usually scene headings and character cues) \
             are in capitals, dialogue is indented two spaces, empty lines are left out, and lines that can't be \
             changed end with [not editable]; lines in a dual dialogue block end with [dual dialogue] and can take text changes but not be added, removed or retyped. Give a scene number or heading text, or a paragraph_id to get the \
             scene it is in.",
            json!({
                "scene": { "type": ["integer", "string"], "description": "Scene number (1 is the first) or part of its heading (ignoring case). A string of digits is a scene number if there is such a scene, otherwise heading text." },
                "paragraph_id": { "type": "integer", "description": "Any paragraph in the scene." },
            }),
            &[],
        ),
        spec(
            "read_script",
            &format!(
                "The script as it reads now, every scene laid out as read_scene does, in parts of at most \
                 max_tokens (default {READ_BUDGET}): the first line says which scenes a part covers and the \
                 from_scene to read the next one. Long: prefer read_scene unless you need all of it."
            ),
            json!({
                "from_scene": { "type": "integer", "minimum": 1, "description": "Start at this scene (1 is the first, and includes any lines before it). Default 1." },
                "max_tokens": { "type": "integer", "minimum": MIN_READ_BUDGET, "description": format!("Most tokens (about four characters each) in this part; always at least one whole scene. Default {READ_BUDGET}.") },
            }),
            &[],
        ),
        spec(
            "list_reviews",
            "The reviews attached to the script, as JSON, with how many suggestions the writer has decided. \
             `default` marks the review suggestions go to. With summary: also suggestions whose line wasn't \
             found, ones that would change nothing visible, and warnings.",
            json!({
                "summary": { "type": "boolean", "description": "Add unmatched and invisible suggestions and warnings." },
            }),
            &[],
        ),
        spec(
            "list_suggestions",
            "A review's suggestions as JSON, numbered from 1 (`n`): status, shape (line, block or insert), the \
             lines it covers (paragraph_ids), their current text, the options, and why; a block's before, current \
             and options are lists of {kind, text}. `entry` is the suggestion's stable position in the review file. `outdated` means the line \
             changed after the suggestion was written. Check before proposing, to avoid duplicates.",
            json!({
                "review": { "type": "string", "description": "An attached review's name or file. Leave out for the default review." },
                "pending_only": { "type": "boolean", "description": "Only suggestions the writer hasn't decided yet." },
            }),
            &[],
        ),
        spec(
            "list_versions",
            "The version lined up with the script (a translation, another draft), as JSON: {\"pair\": null} when \
             there is none. Otherwise both files (`this`, the script called on, and `other`) with their absolute \
             `path`, language and label; `is_version` marks the version, the file that is translated and gets \
             suggestions (`made`: Scriptorium made it), and the top-level `version` is its absolute path. Also \
             the links file, how many rows there are by status, how many need work (`needs`), and rows new in \
             the original.",
            json!({}),
            &[],
        ),
        spec(
            "read_rows",
            &format!(
                "The rows of the script and the version lined up with it: {{\"rows\": [...], \"next_from_row\": \
                 n}}, one row per line: {{row, status, label, needs, note?, this: [{{id, kind, text}}], other: \
                 [{{id, kind, text}}], suggestions: [{{review, n, status}}]}}. `this` is the script called on: \
                 its ids are paragraph_ids for suggest_change. `other` is the file lined up with it: its ids belong \
                 to that file, never pass them as paragraph_id. Empty lines are shown (text \"\"). status: \
                 in_sync, this_changed, other_changed, both_changed, blank (not yet translated), one_sided. \
                 `needs`: the row needs a translation or an update; false once the row has any suggestion \
                 (pending or decided, listed in `suggestions`), and always false on the original. Rows with a \
                 `note`, or nothing on the version's side, can't take a suggestion: they are for the writer. At \
                 most `limit` rows (default {ROWS_PAGE}), fewer if long; `next_from_row` is the from_row of the \
                 next page, null when there are no more rows."
            ),
            json!({
                "scene": { "type": ["integer", "string"], "description": "Only this scene's rows: a scene number (1 is the first) or part of its heading (ignoring case), as read_scene takes it; heading text is looked for in this script's headings, then the other file's." },
                "from_row": { "type": "integer", "minimum": 1, "description": "Start at this row number (rows are numbered from 1 in script order)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_ROWS_PAGE, "description": format!("At most this many rows (default {ROWS_PAGE}).") },
                "needs": { "type": "boolean", "description": "Only rows that need work (on the version)." },
            }),
            &[],
        ),
        spec(
            "new_version",
            "Make a version of the script in another language, lined up with it row by row (a links file beside \
             them): scene headings and character names copied, every other line blank, to fill with suggestions \
             (read_rows, then suggest_changes with row). If it was made before, finds it. Returns the version's \
             absolute path: suggestions go on the version, never on the original. One version per script for now.",
            json!({
                "language": { "type": "string", "description": "The version's language code, e.g. \"es\"." },
                "original_language": { "type": "string", "description": "The script's own language code, e.g. \"en\" (tell from the script)." },
            }),
            &["language", "original_language"],
        ),
        spec(
            "suggest_change",
            "Propose a change: to one line (paragraph_id), to lines that go together (paragraph_ids), or new lines \
             (insert_after); give exactly one of these. The writer accepts an option, keeps their lines, or \
             rewrites them. Checks that the lines exist, are consecutive and can be changed, and that the change \
             would be visible. A line is in at most one suggestion per review. On a version, a suggestion covers \
             one row's lines (read_rows).",
            target.clone(),
            &["options"],
        ),
        spec(
            "suggest_changes",
            &format!(
                "Propose many changes at once (up to {MAX_BATCH}), each like suggest_change (exactly one of \
                 paragraph_id, paragraph_ids or insert_after), written together. Ones that fail are reported and \
                 the rest are added: the result is {{\"added\": n, \"suggestions\": [{{\"index\", \
                 \"paragraph_id\", \"paragraph_ids\" (blocks), \"shape\", \"n\", \"entry\"}}], \"failed\": \
                 [{{\"index\", \"paragraph_id\", \"error\"}}]}}."
            ),
            json!({
                "suggestions": {
                    "type": "array", "minItems": 1, "maxItems": MAX_BATCH,
                    "items": {
                        "type": "object",
                        "properties": target,
                        "required": ["options"],
                        "additionalProperties": false,
                    },
                },
            }),
            &["suggestions"],
        ),
    ]
}

/// Add MCP's `before` to a `suggest_change` spec: one line can then be named
/// by its text instead of `paragraph_id`. No target is required by the
/// schema; the tool checks that exactly one is given.
pub fn add_before(spec: &mut ToolSpec) {
    spec.schema["properties"]["before"] = json!({
        "type": "string",
        "description": "Instead of paragraph_id: the line's current text. Refused if several lines read the same or alike; \
                        then give paragraph_id.",
    });
    if let Some(required) = spec.schema["required"].as_array_mut() {
        required.retain(|r| r != "paragraph_id");
    }
    spec.description.push_str(" Or name one line by its text with before instead of paragraph_id.");
}

/// An argument a host adds to the base schemas for its own use; the tools
/// themselves read only `review_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostField {
    /// `script` (required): which screenplay, for a host serving several
    /// (MCP). The host strips it before [`call`](crate::call).
    Script,
    /// `review_file` (optional): the review file suggestions go to, created
    /// and attached if missing ([`Target::File`](crate::Target::File)). Only
    /// added to `suggest_change` and `suggest_changes`; other specs are left
    /// alone, so a host can pass it for every spec.
    ReviewFile,
}

/// Add host-specific `fields` to `spec`'s schema (beside [`add_before`]).
/// Adding a field twice changes nothing.
pub fn add_host_fields(spec: &mut ToolSpec, fields: &[HostField]) {
    for field in fields {
        let (name, schema, required) = match field {
            HostField::Script => {
                ("script", json!({ "type": "string", "description": "Path to the screenplay (.fdx)." }), true)
            }
            HostField::ReviewFile if matches!(spec.name.as_str(), "suggest_change" | "suggest_changes") => (
                "review_file",
                json!({
                    "type": "string",
                    "description": "The review file to add to, e.g. \"dialogue-pass.review.txt\": a .review.txt \
                                    in the script's folder, created and attached if missing. Suggestions only go \
                                    into review files inside the script's folder. Leave out to use the default \
                                    review.",
                }),
                false,
            ),
            HostField::ReviewFile => continue,
        };
        spec.schema["properties"][name] = schema;
        if required
            && let Some(list) = spec.schema["required"].as_array_mut()
            && !list.iter().any(|r| r == name)
        {
            list.insert(0, json!(name));
        }
    }
}

pub(crate) fn spec(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        schema: json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false }),
    }
}

/// Checks that `args` uses only the arguments `spec` offers: every object
/// whose schema says `additionalProperties: false` (each spec's top level,
/// a batch's items, a line) may hold only the properties listed, so a model
/// can't pass an argument the host didn't offer (the built-in agent isn't
/// offered `review_file`, its host's choice). Every host also confines the
/// files a model names to the script's folder (AGENTS.md rule 6). Types and
/// values are left to the tools, which phrase those errors themselves; an
/// `anyOf` value of no allowed form is refused here.
pub fn check_args(spec: &ToolSpec, args: &Value) -> Result<(), ToolError> {
    let name = &spec.name;
    match unoffered(&spec.schema, args, "") {
        None => Ok(()),
        Some(Finding::Unoffered(at)) => {
            Err(ToolError::Args(format!("{name} doesn't take the argument `{at}`: use only the arguments it lists")))
        }
        Some(Finding::Shape(at, allowed)) => Err(ToolError::Args(format!("{name}: `{at}` must be {allowed}"))),
    }
}

/// What [`unoffered`] found, at a path like `suggestions[2].review_file`.
enum Finding {
    /// An argument the schema doesn't offer.
    Unoffered(String),
    /// A value none of an `anyOf`'s branches takes by type, with what they
    /// take ("a string or a list of lines [{kind, text}]").
    Shape(String, String),
}

/// The first argument in `value` that `schema` doesn't offer, if any.
fn unoffered(schema: &Value, value: &Value, at: &str) -> Option<Finding> {
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        // Allowed if a branch of its type allows it; a value no branch's
        // type fits is refused; else the first finding.
        let fitting: Vec<&Value> = branches.iter().filter(|b| type_fits(b, value)).collect();
        if fitting.is_empty() {
            let allowed: Vec<String> = branches.iter().map(form).collect();
            return Some(Finding::Shape(at.to_string(), allowed.join(" or ")));
        }
        let mut found: Vec<Finding> = fitting.iter().filter_map(|b| unoffered(b, value, at)).collect();
        return if found.len() < fitting.len() { None } else { Some(found.remove(0)) };
    }
    match value {
        Value::Object(map) => {
            let props = schema.get("properties").and_then(Value::as_object);
            let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
            for (k, v) in map {
                let path = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
                match props.and_then(|p| p.get(k)) {
                    Some(sub) => {
                        if let Some(found) = unoffered(sub, v, &path) {
                            return Some(found);
                        }
                    }
                    None if closed => return Some(Finding::Unoffered(path)),
                    None => {}
                }
            }
            None
        }
        Value::Array(items) => {
            let sub = schema.get("items")?;
            items.iter().enumerate().find_map(|(i, v)| unoffered(sub, v, &format!("{at}[{i}]")))
        }
        _ => None,
    }
}

/// What a schema takes, for a person or a model: "a string", "a list of
/// lines [{kind, text}]", "a whole number"...
fn form(schema: &Value) -> String {
    let fields = |s: &Value| {
        s.get("properties").and_then(Value::as_object).map(|p| p.keys().cloned().collect::<Vec<_>>().join(", "))
    };
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => "a string".into(),
        Some("integer") => "a whole number".into(),
        Some("number") => "a number".into(),
        Some("boolean") => "true or false".into(),
        Some("object") => fields(schema).map_or("an object".into(), |f| format!("an object {{{f}}}")),
        Some("array") => match schema.get("items").and_then(fields) {
            Some(f) if f == "kind, text" => "a list of lines [{kind, text}]".into(),
            Some(f) => format!("a list of {{{f}}}"),
            None => "a list".into(),
        },
        _ => "a value of another form".into(),
    }
}

/// Whether `value` is of a type `schema` names (any, if it names none).
fn type_fits(schema: &Value, value: &Value) -> bool {
    let name = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    let fits = |t: &Value| t == name || (t == "number" && name == "integer");
    match schema.get("type") {
        None => true,
        Some(Value::Array(types)) => types.iter().any(fits),
        Some(t) => fits(t),
    }
}
