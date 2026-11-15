//! Names, descriptions and argument schemas. Base schemas: hosts add their own
//! fields (MCP adds `script`, `review_file` and `before`).

pub use scriptorium_agent::ToolSpec;
use serde_json::{Value, json};

/// Default and largest page for `list_paragraphs`: a feature film has a few
/// thousand paragraphs, far more than is useful in one result.
pub const PAGE: u64 = 200;
pub const MAX_PAGE: u64 = 1000;
/// Most suggestions in one `suggest_changes` call.
pub const MAX_BATCH: usize = 200;
/// Most options in one suggestion.
pub const MAX_OPTIONS: usize = 3;

pub fn catalogue() -> Vec<ToolSpec> {
    let options = json!({
        "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": MAX_OPTIONS,
        "description": "One to three replacement texts for the whole line, best first. No newlines.",
    });
    let why = json!({ "type": "string", "description": "A short reason the writer will see." });
    let paragraph_id = json!({ "type": "integer", "description": "The line to change: the number after ¶ in read_scene, or an id from list_paragraphs." });
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
             changed end with [not editable]. Give a scene number or heading text, or a paragraph_id to get the \
             scene it is in.",
            json!({
                "scene": { "type": ["integer", "string"], "description": "Scene number (1 is the first) or part of its heading (ignoring case). A string of digits is a scene number if there is such a scene, otherwise heading text." },
                "paragraph_id": { "type": "integer", "description": "Any paragraph in the scene." },
            }),
            &[],
        ),
        spec(
            "read_script",
            "The whole script as it reads now, every scene laid out as read_scene does. Long: prefer read_scene \
             unless you need all of it.",
            json!({}),
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
            "A review's suggestions as JSON, numbered from 1 (`n`): status, the line's current text, the options, \
             and why. `entry` is the suggestion's stable position in the review file. `outdated` means the line \
             changed after the suggestion was written. Check before proposing, to avoid duplicates.",
            json!({
                "review": { "type": "string", "description": "An attached review's name or file. Leave out for the default review." },
                "pending_only": { "type": "boolean", "description": "Only suggestions the writer hasn't decided yet." },
            }),
            &[],
        ),
        spec(
            "suggest_change",
            "Propose a change to one line. The writer accepts an option, keeps their line, or rewrites it. Checks \
             that the line exists and can be changed, and that the change would be visible. One suggestion per line \
             per review.",
            json!({ "paragraph_id": paragraph_id, "options": options, "why": why }),
            &["paragraph_id", "options"],
        ),
        spec(
            "suggest_changes",
            &format!(
                "Propose changes to several lines at once (up to {MAX_BATCH}), checked like suggest_change and \
                 written together. Lines that fail are reported and the rest are added: the result is \
                 {{\"added\": n, \"suggestions\": [{{\"index\", \"paragraph_id\", \"n\", \"entry\"}}], \
                 \"failed\": [{{\"index\", \"paragraph_id\", \"error\"}}]}}."
            ),
            json!({
                "suggestions": {
                    "type": "array", "minItems": 1, "maxItems": MAX_BATCH,
                    "items": {
                        "type": "object",
                        "properties": { "paragraph_id": paragraph_id, "options": options, "why": why },
                        "required": ["paragraph_id", "options"],
                        "additionalProperties": false,
                    },
                },
            }),
            &["suggestions"],
        ),
    ]
}

/// Add MCP's `before` to a `suggest_change` spec: the line can then be named
/// by its text instead of `paragraph_id`, so neither is required (the tool
/// checks that exactly one is given). The base schema requires
/// `paragraph_id`, which is all the built-in agent uses.
pub fn add_before(spec: &mut ToolSpec) {
    spec.schema["properties"]["before"] = json!({
        "type": "string",
        "description": "Instead of paragraph_id: the line's current text. Refused if several lines read the same or alike; \
                        then give paragraph_id.",
    });
    if let Some(required) = spec.schema["required"].as_array_mut() {
        required.retain(|r| r != "paragraph_id");
    }
    spec.description.push_str(" Give exactly one of paragraph_id or before.");
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
                    "description": "The review file to add to, e.g. \"dialogue-pass.review.txt\" (created and \
                                    attached if missing). Leave out to use the default review.",
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

fn spec(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        schema: json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false }),
    }
}
