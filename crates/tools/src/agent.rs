//! The tools offered to outside agents ([`Offer::Agent`](crate::Offer::Agent):
//! MCP, the CLI `tool`), beyond the chat catalogue: the shared tools with
//! the host fields (`script`, `review_file`, `before`), and the tools only
//! agents get (attach and detach reviews, decide, edit, undo, redo, write
//! the script), and `guide` ([`crate::guidance`]). Also each tool's MCP
//! annotations.
//!
//! Paths: `script` is optional (left out, the host uses the script open in
//! the app); given, it is an absolute path. Every other path a tool names is
//! relative to the script's folder, and confined to it by the host (AGENTS.md rule 6).

use serde_json::{Value, json};

use crate::catalogue::{HostField, KINDS, ToolSpec, add_before, add_host_fields, spec};

const SCRIPT: &str = "Leave out to use the script open in Scriptorium; otherwise its absolute path (.fdx).";
const REVIEW: &str = "Which attached review to use, by name or file (a path relative to the script's folder). Leave \
                      out when only one review is attached to the script.";
const REVIEW_FILE: &str = "The review file to add to, e.g. \"dialogue-pass.review.txt\": a .review.txt file in the \
                           script's folder (relative to it, or absolute), created and attached if missing. \
                           Suggestions only go into review files inside the script's folder; one there that \
                           doesn't start with SCRIPT REVIEW must be attached by the writer first. Always give one, \
                           named for the pass, and keep it for the whole pass.";
const FILES: &str = "Review files (.review.txt) in the script's folder: paths relative to it, or absolute.";
const ATTACHED: &str = "Attached review files: paths relative to the script's folder, absolute, or as list_reviews \
                        shows them.";

/// The tools that only read: the host takes no lock for them, and they are
/// annotated `readOnlyHint: true`.
const READ_ONLY: &[&str] = &[
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

/// The tools only [`Offer::Agent`](crate::Offer::Agent) has, in the order
/// they are offered. Each one changes the session or a file.
pub const AGENT_ONLY: [&str; 7] =
    ["attach_reviews", "detach_reviews", "decide_suggestion", "edit_paragraph", "undo", "redo", "write_script"];

/// Whether tool `name` only reads (a tool of either offer). Unknown names
/// don't.
pub fn read_only(name: &str) -> bool {
    READ_ONLY.contains(&name)
}

/// The MCP annotations of tool `name` (`readOnlyHint` and friends), or
/// `None` for a name no offer has. Read tools are read-only; every other
/// tool can be undone (`destructiveHint: false`) except `write_script`,
/// which may replace a file.
pub fn annotations(name: &str) -> Option<Value> {
    if read_only(name) {
        Some(json!({ "readOnlyHint": true }))
    } else if name == "write_script" {
        Some(json!({ "readOnlyHint": false, "destructiveHint": true }))
    } else if crate::writes(name) {
        Some(json!({ "readOnlyHint": false, "destructiveHint": false }))
    } else {
        None
    }
}

/// `spec` as an MCP `tools/list` entry: name, description, `inputSchema`
/// and its [`annotations`].
pub fn mcp_tool(spec: &ToolSpec) -> Value {
    let mut tool = json!({ "name": spec.name, "description": spec.description, "inputSchema": spec.schema });
    if let Some(a) = annotations(&spec.name) {
        tool["annotations"] = a;
    }
    tool
}

/// The chat catalogue `base` with the agent host fields, then the
/// agent-only tools.
pub(crate) fn offer(base: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let mut specs = base;
    for spec in &mut specs {
        if spec.name == "suggest_change" {
            add_before(spec);
        }
        add_host_fields(spec, &[HostField::Script, HostField::ReviewFile]);
        crate::guidance::add_hint(spec);
        spec.schema["properties"]["script"]["description"] = json!(SCRIPT);
        if let Some(required) = spec.schema["required"].as_array_mut() {
            required.retain(|r| r != "script");
        }
        if spec.schema["properties"].get("review_file").is_some() {
            spec.schema["properties"]["review_file"]["description"] = json!(REVIEW_FILE);
        }
    }
    specs.extend(agent_only());
    specs.push(crate::guidance::guide_spec());
    specs
}

fn agent_only() -> Vec<ToolSpec> {
    let script = json!({ "type": "string", "description": SCRIPT });
    let review = json!({ "type": "string", "description": REVIEW });
    let files = |description: &str| json!({ "type": "array", "items": { "type": "string" }, "minItems": 1, "description": description });
    vec![
        spec(
            "attach_reviews",
            "Attach review files (.review.txt files in the script's folder that start with SCRIPT REVIEW) to the \
             script so they show up in the app and in these tools. suggest_change and suggest_changes attach their \
             review_file themselves. Files elsewhere are attached by the writer in the app.",
            json!({ "script": script, "files": files(FILES) }),
            &["files"],
        ),
        spec(
            "detach_reviews",
            "Detach review files from the script. Decisions already made stay in the script's text.",
            json!({ "script": script, "files": files(ATTACHED) }),
            &["files"],
        ),
        spec(
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
                        "properties": { "kind": { "type": "string", "enum": KINDS }, "text": { "type": "string" } },
                        "required": ["kind", "text"],
                        "additionalProperties": false,
                    },
                    "description": "With rewrite of a block suggestion: its new lines, {kind, text} each \
                                    (kind: Scene Heading, Action, Character, Parenthetical, Dialogue, Transition, \
                                    Shot or General). an option `[]` cuts the lines (give `lines: []`).",
                },
            }),
            &["n", "decision"],
        ),
        spec(
            "edit_paragraph",
            "Replace one paragraph's whole text directly (no review). One paragraph, no newlines.",
            json!({
                "script": script,
                "paragraph_id": { "type": "integer" },
                "text": { "type": "string" },
            }),
            &["paragraph_id", "text"],
        ),
        spec(
            "undo",
            "Undo the last change to the script (an edit or a decision, made here or in the app).",
            json!({ "script": script }),
            &[],
        ),
        spec("redo", "Redo the last undone change.", json!({ "script": script }), &[]),
        spec(
            "write_script",
            "Write the script with every decision and edit so far to a new .fdx (default: \"<name> (scriptorium).fdx\" \
             next to the original, replaced without warning if it exists, so the writer shouldn't keep editing \
             that file). The original, and any other existing .fdx, is never overwritten: give a new file name. \
             For a version Scriptorium made (new_version), the writer saves it in the app with Write .fdx; here, \
             give `output`.",
            json!({
                "script": script,
                "output": { "type": "string", "description": "Where to write, instead of the default: a .fdx in the script's folder (relative to it, or absolute)." },
            }),
            &[],
        ),
    ]
}
