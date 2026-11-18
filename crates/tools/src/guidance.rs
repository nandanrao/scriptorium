//! What outside agents are told about working with Scriptorium, in one
//! place: the server `instructions`, the `guide` tool (the full guidance for
//! a review pass or a translation, `guidance/*.md`), the hints the agent
//! offer adds to tool descriptions, and the MCP prompts (`review_pass`,
//! `translate`), whose text is the guide's.
//!
//! Every client reaches the `guide` tool; whether it reads the instructions
//! or offers prompts varies, so the instructions and the descriptions point
//! at the guide.

use serde_json::{Value, json};

use crate::ToolError;
use crate::catalogue::{ToolSpec, spec};

/// The server `instructions` an agent gets when it connects. The first
/// paragraph (at most 512 characters) stands alone, as Codex shows only that
/// much; every rule that matters is in the first 2 KB (older Claude Code);
/// the whole is under 4,096.
pub const INSTRUCTIONS: &str = "\
Scriptorium is the writer's screenplay app (Final Draft .fdx). Call where_is_writer first: it says which script is \
open and where the writer is. Propose, don't edit: put changes into a review file with suggest_changes, and the \
writer decides each one in the app. Before a review pass call guide('review'); before translating call \
guide('translate'). When the writer asks for feedback rather than changes, answer in chat.

Rules:
- Always give review_file when proposing, named for the pass (e.g. dialogue-pass.review.txt), the same file for \
the whole pass.
- Never edit the .fdx yourself. decide_suggestion, edit_paragraph, undo, redo and write_script only when the \
writer asks you to. undo undoes the last step, whoever made it, so it can undo the writer's typing.
- script: leave it out to work on the script open in Scriptorium. For another script pass its absolute path; if \
you don't know where it is, ask the writer for its path, or to open it in Scriptorium.
- Reads show the script as it is now, with the writer's decisions and typing: read again rather than trusting an \
earlier read.

To translate (after guide('translate')): list_versions, then new_version makes (or finds) the version and returns \
its absolute path, used as script from then on; read_rows by scene, then suggest_changes with row (each item \
covering all of its row's this ids) into \"Translation from <the original's label from list_versions>.review.txt\"; \
updates go in the same review.

To build an app (a small web page on the script), call guide('app') first.";

/// What the `guide` tool explains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// Running a review pass: reading, proposing, deciding when asked.
    Review,
    /// Making a version in another language and translating it.
    Translate,
    /// Building an app: a small web page on the script, in the writer's
    /// browser.
    App,
}

impl Topic {
    /// Every topic, in the order the `guide` schema lists them.
    pub const ALL: [Topic; 3] = [Topic::Review, Topic::Translate, Topic::App];

    /// The topic's name, as `guide` takes it.
    pub fn name(self) -> &'static str {
        match self {
            Topic::Review => "review",
            Topic::Translate => "translate",
            Topic::App => "app",
        }
    }

    /// The topic named `name`, if any.
    pub fn from_name(name: &str) -> Option<Topic> {
        Topic::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The full guidance (Markdown).
    pub fn text(self) -> &'static str {
        match self {
            Topic::Review => include_str!("guidance/review.md"),
            Topic::Translate => include_str!("guidance/translate.md"),
            Topic::App => include_str!("guidance/app.md"),
        }
    }
}

/// The `guide` tool's spec: read-only, the one agent tool without `script`.
pub fn guide_spec() -> ToolSpec {
    let topics: Vec<&str> = Topic::ALL.iter().map(|t| t.name()).collect();
    spec(
        "guide",
        "How to work with Scriptorium, in full: call guide('review') before a review pass (reading, proposing, \
         naming the review file, deciding only when asked), and guide('translate') before translating (making the \
         version, the brief and glossary, rows, updates), and guide('app') before building an app (a small web page on \
         the script). Read-only; works without a script.",
        json!({
            "topic": { "type": "string", "enum": topics, "description": "review: a pass of changes on a script. translate: a version in another language. app: a web page on the script." },
        }),
        &["topic"],
    )
}

/// Run `guide` with `args` (`{"topic": "review"}`): the topic's text.
pub fn guide(args: &Value) -> Result<String, ToolError> {
    let topic = args
        .get("topic")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Args("guide needs `topic`: \"review\", \"translate\" or \"app\"".into()))?;
    Topic::from_name(topic).map(|t| t.text().to_string()).ok_or_else(|| {
        ToolError::Args(format!("guide: unknown topic {topic:?}; use \"review\", \"translate\" or \"app\""))
    })
}

/// Points tool `spec`'s description at the guide where it matters (agent
/// offer only: a client may drop the instructions). The suggesting tools
/// say to read guide('review') before a pass; `new_version` and `read_rows`
/// to read guide('translate') before translating. Other specs are left
/// alone.
pub(crate) fn add_hint(spec: &mut ToolSpec) {
    let hint = match spec.name.as_str() {
        "suggest_change" | "suggest_changes" => {
            " Before a pass, call guide('review'). Always give review_file, named for the pass (e.g. \
             dialogue-pass.review.txt), the same for the whole pass (a translation: \
             `Translation from <label>.review.txt`)."
        }
        "new_version" | "read_rows" => " Call guide('translate') before translating.",
        _ => return,
    };
    spec.description.push_str(hint);
}

/// An MCP prompt: its name, the topic whose text it sends, a description,
/// and its arguments (all optional).
struct PromptDef {
    name: &'static str,
    topic: Topic,
    title: &'static str,
    description: &'static str,
    arguments: &'static [(&'static str, &'static str)],
}

const PROMPTS: [PromptDef; 2] = [
    PromptDef {
        name: "review_pass",
        topic: Topic::Review,
        title: "Review pass",
        description: "Run a review pass on the script: propose changes for the writer to decide.",
        arguments: &[("scene", "Only this scene: its number (1 is the first) or part of its heading.")],
    },
    PromptDef {
        name: "translate",
        topic: Topic::Translate,
        title: "Translate",
        description: "Translate the script into another language, as a version lined up with it.",
        arguments: &[
            ("language", "The language to translate into, e.g. Spanish or es."),
            ("scene", "Only this scene: its number (1 is the first) or part of its heading."),
        ],
    },
];

/// The MCP `prompts/list` entries.
pub fn prompts() -> Vec<Value> {
    PROMPTS
        .iter()
        .map(|p| {
            let arguments: Vec<Value> = p
                .arguments
                .iter()
                .map(|(name, description)| json!({ "name": name, "description": description, "required": false }))
                .collect();
            json!({ "name": p.name, "title": p.title, "description": p.description, "arguments": arguments })
        })
        .collect()
}

/// The MCP `prompts/get` result for prompt `name` with `arguments` (an
/// object of strings, or null): a first user message with the guide's text
/// for the prompt's topic, then one asking for the task with the arguments
/// given. `ToolError::Unknown` for a name no prompt has.
pub fn prompt(name: &str, arguments: &Value) -> Result<Value, ToolError> {
    let p = PROMPTS.iter().find(|p| p.name == name).ok_or_else(|| ToolError::Unknown(name.to_string()))?;
    let arg = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let mut task = match (p.topic, arg("language")) {
        (Topic::Review, _) => "Run a review pass on the script".to_string(),
        (Topic::App, _) => "Build an app on the script".to_string(),
        (Topic::Translate, Some(language)) => format!("Translate the script into {language}"),
        (Topic::Translate, None) => "Translate the script (ask me into which language)".to_string(),
    };
    match arg("scene") {
        Some(scene) => task.push_str(&format!(", scene {scene}, following the guidance above.")),
        None => task.push_str(", following the guidance above."),
    }
    let message = |text: &str| json!({ "role": "user", "content": { "type": "text", "text": text } });
    Ok(json!({ "description": p.description, "messages": [message(p.topic.text()), message(&task)] }))
}
