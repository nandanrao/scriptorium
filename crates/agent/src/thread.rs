//! A conversation, stored independently of any provider.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AgentError;
use crate::protocol::Protocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// Results of the tools the previous assistant message called.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// Text. Whitespace-only text is kept in storage but never sent to
    /// Anthropic, which refuses empty text blocks in every role.
    Text {
        text: String,
    },
    /// The model's reasoning. `signature` is set when the provider needs the
    /// block sent back unchanged (Anthropic); it is sent back only to the
    /// endpoint and model that produced it (see [`Message::origin`]), and
    /// blocks without one are shown but never sent back.
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    /// A block only its own protocol understands (e.g. Anthropic's
    /// `redacted_thinking`), replayed verbatim to the endpoint and model that
    /// produced it and dropped for any other.
    Native {
        protocol: Protocol,
        value: Value,
    },
    /// For the writer only: shown in the thread, never sent to any provider
    /// (a refusal, a stopped reply, a cancelled tool run).
    Notice {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    /// Set when the streamed arguments weren't valid JSON (e.g. cut off at the
    /// output limit). The tool must not run; tell the model instead
    /// ([`crate::turn::Run`] does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_error: Option<String>,
    /// The argument text exactly as streamed, kept only when it didn't parse
    /// (`input` is then `Null`). Not replayed: both protocols need valid JSON
    /// there, so requests send `{}` in its place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<String>,
    /// Provider data attached to the call that must be sent back with it,
    /// opaque to us. Gemini's OpenAI endpoint puts its thought signature here
    /// (`extra_content: {google: {thought_signature}}`) and answers 400 when a
    /// later request omits it. Replayed verbatim as `extra_content`, only on
    /// the OpenAI protocol and only to the message's own origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

/// Where an assistant reply came from. Provider replay data (signatures,
/// encrypted reasoning) is only valid for the same endpoint and model.
///
/// So switching endpoint or model mid-tool-loop drops the reply's signed
/// thinking; with adaptive thinking some Anthropic API versions reject a
/// `tool_use` turn without it in an active loop (see
/// [`crate::protocol::build_request`]). Switch between runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Origin {
    /// The base URL, without a trailing slash.
    pub endpoint: String,
    pub model: String,
}

impl Origin {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Origin { endpoint: endpoint.trim_end_matches('/').to_string(), model: model.to_string() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
    /// Assistant replies: the endpoint and model that wrote it (set by
    /// [`Thread::push_turn`]). `None` for other messages and older files;
    /// replay data of a reply without one is never sent back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Assistant replies: what the reply cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl Message {
    pub fn new(role: Role, blocks: Vec<Block>) -> Self {
        Message { role, blocks, origin: None, usage: None }
    }

    pub fn user(text: &str) -> Self {
        Message::new(Role::User, vec![Block::Text { text: text.to_string() }])
    }

    pub fn assistant(blocks: Vec<Block>) -> Self {
        Message::new(Role::Assistant, blocks)
    }

    /// The text blocks, joined (notices excluded).
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<&ToolCall> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolCall(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    pub fn tool_results(&self) -> Vec<&ToolResult> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult(r) => Some(r),
                _ => None,
            })
            .collect()
    }

    /// Only notices (or nothing): shown to the writer, invisible to providers
    /// and to the checks on what may be sent.
    pub fn is_display_only(&self) -> bool {
        self.blocks.iter().all(|b| matches!(b, Block::Notice { .. }))
    }
}

/// Token counts (and cost, when the provider says), as the provider reports them.
///
/// `input` counts every input token: `cached` and `cache_write` are parts of
/// it, not additions to it. To price a reply, charge `input - cached -
/// cache_write` at the base input rate, `cached` at the cache-read rate and
/// `cache_write` at the cache-write rate ([`Usage::uncached_input`] does the
/// subtraction without underflowing; see [`Thread::replies`] for the model
/// each reply ran on). The parsers keep `cached + cache_write <= input`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// All input tokens, cache reads and writes included.
    pub input: u64,
    pub output: u64,
    /// Input tokens read from the provider's prompt cache (included in `input`).
    #[serde(default)]
    pub cached: u64,
    /// Input tokens written to the prompt cache (Anthropic's
    /// `cache_creation_input_tokens`; included in `input`).
    #[serde(default)]
    pub cache_write: u64,
    /// Output tokens spent reasoning, when the provider says (included in `output`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// False when the provider didn't report (all) the counts, e.g. a stream
    /// that ended without its usage chunk, or a stopped reply. A total is
    /// known only if every part is.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub known: bool,
    /// What the reply cost, in the provider's currency (US dollars for
    /// OpenRouter's `usage.cost`), when the provider reports it. A sum has a
    /// cost only if every part has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl Default for Usage {
    fn default() -> Self {
        Usage { input: 0, output: 0, cached: 0, cache_write: 0, reasoning: None, known: true, cost: None }
    }
}

impl Usage {
    /// Nothing reported.
    pub fn unknown() -> Self {
        Usage { known: false, ..Usage::default() }
    }

    /// Input tokens charged at the base rate: `input - cached - cache_write`,
    /// never below 0 (even for counts a provider got wrong).
    pub fn uncached_input(&self) -> u64 {
        self.input.saturating_sub(self.cached).saturating_sub(self.cache_write)
    }

    pub fn add(&mut self, o: Usage) {
        self.input = self.input.saturating_add(o.input);
        self.output = self.output.saturating_add(o.output);
        self.cached = self.cached.saturating_add(o.cached);
        self.cache_write = self.cache_write.saturating_add(o.cache_write);
        self.reasoning = match (self.reasoning, o.reasoning) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
        };
        self.known &= o.known;
        self.cost = match (self.cost, o.cost) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its reply.
    EndTurn,
    /// The model is waiting for tool results.
    ToolUse,
    /// Cut off at the output limit.
    MaxTokens,
    /// The provider declined to answer.
    Refusal,
    /// The writer stopped the reply ([`crate::ReplyParser::abort`]).
    Cancelled,
    Other(String),
}

/// What a reply stream reports while it arrives, for showing progress.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Reply text. `index` says which block it belongs to: pieces with the
    /// same index continue one block (Anthropic's content block index; for
    /// the OpenAI protocol, a counter that moves on whenever the stream
    /// switches between text, reasoning and tool calls).
    Text {
        index: usize,
        text: String,
    },
    /// Reasoning text, indexed like [`Event::Text`].
    Thinking {
        index: usize,
        text: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
    },
    /// A tool call's arguments are arriving (`id` may be empty when the
    /// provider sends none; ids are made unique by [`Thread::push_turn`]).
    ToolArgs {
        id: String,
        bytes_so_far: usize,
    },
    /// The usage so far (it arrives in pieces; the last one is the total).
    Usage(Usage),
    Stop(StopReason),
}

/// One finished assistant reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub message: Message,
    pub stop: StopReason,
    pub usage: Usage,
    /// Why the provider declined, when it says (OpenAI's `refusal` text,
    /// Anthropic's `stop_details.explanation`). Stored in the thread as part of
    /// the refusal's notice.
    pub refusal: Option<String>,
}

/// What [`Thread::push_turn`] leaves for the caller to do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pushed {
    /// Tool calls to answer before the next turn (empty when the reply is
    /// complete, refused or stopped). Ids are unique within the thread.
    pub calls: Vec<ToolCall>,
    /// After a refusal: the writer's message that triggered it, taken out of
    /// the thread (as the provider asks) so the UI can offer it back for
    /// rephrasing.
    pub withdrawn: Option<String>,
}

/// Notice stored for a reply the writer stopped.
pub const STOPPED: &str = "Stopped before the reply finished.";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    pub messages: Vec<Message>,
}

/// Whether a provider would see `m` in a request to a target that wrote none
/// of the thread (see [`crate::protocol::is_sent`]). Used where no request
/// is being built (finding waiting calls): there the messages that matter
/// carry tool calls or results, which every target sees.
fn is_sendable(m: &Message) -> bool {
    // With no target, no replay data counts, so the protocol makes no difference.
    crate::protocol::is_sent(Protocol::Anthropic, None, m)
}

impl Thread {
    /// Adds the writer's message. Returns `false`, leaving the thread
    /// unchanged, when `text` is blank (empty or whitespace): providers
    /// refuse empty text, and the request would end on the assistant's turn.
    ///
    /// Tool calls still waiting for results (a run stopped or crashed before
    /// its tools ran) are first answered as cancelled ([`crate::turn::cancel`]),
    /// so a typed message never leaves calls stranded behind it.
    ///
    /// Don't call this while a [`crate::turn::Run`] is active
    /// ([`crate::turn::Run::is_active`]): lock the composer instead. The calls
    /// the run's tools are answering would be cancelled under it, and the
    /// run's results then refused ([`AgentError::Mismatch`]).
    #[must_use = "false means the text was blank and nothing was added"]
    pub fn push_user(&mut self, text: &str) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        crate::turn::cancel(self);
        self.messages.push(Message::user(text));
        true
    }

    /// Everything the thread has cost: the sum of every reply's usage
    /// (including replies withdrawn after a refusal).
    pub fn usage(&self) -> Usage {
        let mut total: Option<Usage> = None;
        for u in self.messages.iter().filter_map(|m| m.usage.as_ref()) {
            match &mut total {
                Some(t) => t.add(*u),
                None => total = Some(*u),
            }
        }
        total.unwrap_or_default()
    }

    /// Each reply's endpoint and model with its usage, oldest first, for
    /// pricing a thread that ran on more than one model.
    pub fn replies(&self) -> impl Iterator<Item = (&Origin, &Usage)> {
        self.messages.iter().filter_map(|m| Some((m.origin.as_ref()?, m.usage.as_ref()?)))
    }

    /// Adds a finished reply written by `origin` (see [`crate::Call::origin`]),
    /// with its usage. Returns what to do next.
    ///
    /// - Tool calls are returned to run. Ids missing or already used in the
    ///   thread are replaced by unique ones (some servers send none, or reuse
    ///   `call_0` every turn).
    /// - A refused reply is not kept: in its place goes a notice (with its
    ///   origin and usage). The provider asks for the turn that led to the
    ///   refusal to be removed or rephrased before continuing, so the thread
    ///   goes back to before the writer's last message: that message is taken
    ///   out and returned in [`Pushed::withdrawn`], along with the tool loop
    ///   that followed it. The withdrawn replies keep their origin and usage
    ///   (as empty, display-only messages) so the thread's cost still counts
    ///   them. With no writer's message to withdraw, earlier messages stay.
    /// - A stopped reply ([`StopReason::Cancelled`]) keeps its text, loses
    ///   any tool calls, and gets a notice.
    pub fn push_turn(&mut self, turn: Turn, origin: Origin) -> Pushed {
        let Turn { mut message, stop, usage, refusal } = turn;
        message.role = Role::Assistant;
        message.origin = Some(origin);
        message.usage = Some(usage);
        match stop {
            StopReason::Refusal => {
                let withdrawn = self.withdraw_last_user_message();
                let text = match refusal {
                    Some(why) if !why.trim().is_empty() => format!("The model declined to answer: {}", why.trim()),
                    _ => "The model declined to answer.".to_string(),
                };
                message.blocks = vec![Block::Notice { text }];
                self.messages.push(message);
                Pushed { calls: vec![], withdrawn }
            }
            StopReason::Cancelled => {
                message.blocks.retain(|b| matches!(b, Block::Text { .. }));
                message.blocks.push(Block::Notice { text: STOPPED.into() });
                self.messages.push(message);
                Pushed::default()
            }
            // Some providers say "stop" while still calling tools; the calls decide.
            _ => {
                self.make_ids_unique(&mut message);
                let calls = message.tool_calls().into_iter().cloned().collect();
                self.messages.push(message);
                Pushed { calls, withdrawn: None }
            }
        }
    }

    fn make_ids_unique(&self, message: &mut Message) {
        let mut used: std::collections::HashSet<String> = self
            .messages
            .iter()
            .flat_map(|m| m.blocks.iter())
            .filter_map(|b| match b {
                Block::ToolCall(c) => Some(c.id.clone()),
                Block::ToolResult(r) => Some(r.id.clone()),
                _ => None,
            })
            .collect();
        let turn = self.messages.len();
        for (i, b) in message.blocks.iter_mut().enumerate() {
            let Block::ToolCall(c) = b else { continue };
            if c.id.is_empty() || used.contains(&c.id) {
                let stem = if c.id.is_empty() { format!("call_{i}") } else { c.id.clone() };
                let mut id = format!("{stem}_t{turn}");
                let mut n = 1;
                while used.contains(&id) {
                    n += 1;
                    id = format!("{stem}_t{turn}_{n}");
                }
                c.id = id;
            }
            used.insert(c.id.clone());
        }
    }

    /// Takes out the writer's last message and everything after it, for a
    /// refusal. Replies among them are kept as empty messages with their
    /// origin and usage, so what they cost still counts. `None` (thread
    /// unchanged) if there is no writer's message with text.
    fn withdraw_last_user_message(&mut self) -> Option<String> {
        let i = self.messages.iter().rposition(|m| m.role == Role::User && !m.text().trim().is_empty())?;
        let removed = self.messages.split_off(i);
        let text = removed[0].text();
        for m in removed.into_iter().skip(1) {
            if m.role == Role::Assistant && m.usage.is_some() {
                self.messages.push(Message { blocks: vec![], ..m });
            }
        }
        Some(text)
    }

    /// Adds the results of the tool calls waiting for them (see
    /// [`Thread::unanswered_calls`]), in the order of the calls, directly
    /// after the reply that made them. Refused unless there is exactly one
    /// result for each waiting call.
    pub fn push_tool_results(&mut self, mut results: Vec<ToolResult>) -> Result<(), AgentError> {
        let Some((at, calls)) = self.pending() else {
            return Err(AgentError::Mismatch("no tool calls are waiting for results".into()));
        };
        let calls: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
        let mut got: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        let mut want: Vec<&str> = calls.iter().map(String::as_str).collect();
        got.sort_unstable();
        want.sort_unstable();
        if got != want {
            return Err(AgentError::Mismatch(format!(
                "the results ({}) don't match the calls ({})",
                got.join(", "),
                want.join(", ")
            )));
        }
        results.sort_by_key(|r| calls.iter().position(|c| *c == r.id));
        self.answer(at, results, None);
        Ok(())
    }

    /// Puts `results` for the calls of the reply at `at` right after it: into
    /// the tool message already answering it in part, or a new one. Results
    /// stay in the order of the calls; `notice` goes after them.
    pub(crate) fn answer(&mut self, at: usize, results: Vec<ToolResult>, notice: Option<String>) {
        let order: Vec<String> = self.messages[at].tool_calls().iter().map(|c| c.id.clone()).collect();
        let next = self.next_sendable(at).filter(|&n| self.messages[n].role == Role::Tool);
        let tool = match next {
            Some(n) => &mut self.messages[n],
            None => {
                self.messages.insert(at + 1, Message::new(Role::Tool, vec![]));
                &mut self.messages[at + 1]
            }
        };
        tool.blocks.extend(results.into_iter().map(Block::ToolResult));
        // Results first (in call order), then the rest (writer's text, notices) as they were.
        let pos = |b: &Block| match b {
            Block::ToolResult(r) => order.iter().position(|id| *id == r.id).unwrap_or(order.len()),
            _ => order.len() + 1,
        };
        tool.blocks.sort_by_key(pos);
        if let Some(text) = notice {
            tool.blocks.push(Block::Notice { text });
        }
    }

    /// Index of the next message after `i` a provider would see.
    fn next_sendable(&self, i: usize) -> Option<usize> {
        (i + 1..self.messages.len()).find(|&n| is_sendable(&self.messages[n]))
    }

    /// Checks the thread can be sent as it is in a request in `protocol` to
    /// `target` ([`crate::Call::origin`]): what [`crate::protocol::build_request`]
    /// and [`crate::turn::Run`] check. [`AgentError::NotReady`] says why not.
    ///
    /// Only the messages a request to `target` includes count
    /// ([`crate::protocol::is_sent`]: display-only messages, writer's messages
    /// with no text, and replies with nothing for this target are left out;
    /// replay data such as signed thinking goes only to the origin that wrote
    /// it). Those messages must:
    /// - exist, and not end on the assistant's: that is a prefill, which
    ///   current models reject. Answer [`Thread::unanswered_calls`] (or add the
    ///   writer's message) first.
    /// - answer every assistant message with tool calls by the very next
    ///   message, a tool message with one result for each of those calls and
    ///   no others.
    /// - keep blocks in their role: tool calls only in assistant messages,
    ///   tool results only in tool messages.
    pub fn check_sendable(&self, protocol: Protocol, target: &Origin) -> Result<(), AgentError> {
        self.check(|m| crate::protocol::is_sent(protocol, Some(target), m))
    }

    /// [`Thread::check_sendable`] for a target that wrote none of the thread
    /// (no replay data counts). Only tests need it: every request has a target.
    #[cfg(test)]
    pub(crate) fn check_sendable_anywhere(&self) -> Result<(), AgentError> {
        self.check(is_sendable)
    }

    fn check(&self, is_sent: impl Fn(&Message) -> bool) -> Result<(), AgentError> {
        let sendable: Vec<&Message> = self.messages.iter().filter(|m| is_sent(m)).collect();
        let not_ready = |why: String| Err(AgentError::NotReady(why));
        let Some(last) = sendable.last() else { return not_ready("the thread is empty".into()) };
        if last.role == Role::Assistant {
            return not_ready(if last.tool_calls().is_empty() {
                "the thread ends on the assistant's message".into()
            } else {
                "tool calls are waiting for their results".into()
            });
        }
        for (i, m) in sendable.iter().enumerate() {
            let calls = m.tool_calls();
            let results = m.tool_results();
            if !calls.is_empty() && m.role != Role::Assistant {
                return not_ready(format!("message {} has tool calls but isn't the assistant's", i + 1));
            }
            if !results.is_empty() && m.role != Role::Tool {
                return not_ready(format!("message {} has tool results but isn't a tool message", i + 1));
            }
            if !calls.is_empty() {
                let mut want: Vec<&str> = calls.iter().map(|c| c.id.as_str()).collect();
                want.sort_unstable();
                if want.windows(2).any(|w| w[0] == w[1]) {
                    return not_ready(format!("message {} repeats a tool call id", i + 1));
                }
                let next = sendable.get(i + 1).filter(|n| n.role == Role::Tool);
                let mut got: Vec<&str> =
                    next.map(|n| n.tool_results().iter().map(|r| r.id.as_str()).collect()).unwrap_or_default();
                got.sort_unstable();
                if got != want {
                    return not_ready(format!(
                        "the tool calls in message {} ({}) aren't answered by the next message ({})",
                        i + 1,
                        want.join(", "),
                        if next.is_some() { got.join(", ") } else { "no tool results".into() }
                    ));
                }
            }
            if !results.is_empty() && (i == 0 || sendable[i - 1].tool_calls().is_empty()) {
                return not_ready(format!("message {} has tool results for no tool calls", i + 1));
            }
        }
        Ok(())
    }

    /// Tool calls that have no result yet: those of the earliest reply whose
    /// calls aren't all answered by the message after it. Normally that is
    /// the last reply, while its tools run; after a crash (or a file written
    /// mid-run) it may be an earlier one. Answer them before the next request
    /// ([`Thread::push_tool_results`]; [`crate::turn::cancel`] answers every
    /// waiting call as cancelled).
    pub fn unanswered_calls(&self) -> Vec<&ToolCall> {
        self.pending().map(|(_, calls)| calls).unwrap_or_default()
    }

    /// The earliest reply with calls lacking results: its index and those calls.
    pub(crate) fn pending(&self) -> Option<(usize, Vec<&ToolCall>)> {
        self.messages.iter().enumerate().find_map(|(i, m)| {
            if m.role != Role::Assistant || !is_sendable(m) {
                return None;
            }
            let answered: Vec<&str> = match self.next_sendable(i).map(|n| &self.messages[n]) {
                Some(n) if n.role == Role::Tool => n.tool_results().iter().map(|r| r.id.as_str()).collect(),
                _ => vec![],
            };
            let waiting: Vec<&ToolCall> =
                m.tool_calls().into_iter().filter(|c| !answered.contains(&c.id.as_str())).collect();
            (!waiting.is_empty()).then_some((i, waiting))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_untargeted_check_ignores_replay_data_only_its_origin_sees() {
        let mut t = Thread::default();
        assert!(t.push_user("Hi"));
        let mut reply = Message::assistant(vec![Block::Thinking { text: "t".into(), signature: Some("s".into()) }]);
        reply.origin = Some(Origin::new("u", "m"));
        t.messages.push(reply);
        assert!(t.check_sendable_anywhere().is_ok());
        assert!(t.check_sendable(Protocol::OpenaiChat, &Origin::new("u", "m")).is_ok());
        assert!(matches!(t.check_sendable(Protocol::Anthropic, &Origin::new("u", "m")), Err(AgentError::NotReady(_))));
        t.messages.clear();
        assert!(matches!(t.check_sendable_anywhere(), Err(AgentError::NotReady(m)) if m.contains("empty")));
    }
}
