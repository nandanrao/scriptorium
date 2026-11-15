//! The agent loop's pure steps, shared by the CLI and the panel.
//!
//! [`Run`] drives one run of the agent; the shell does the I/O it asks for:
//!
//! ```ignore
//! if !thread.push_user(&text) { return; }           // blank: nothing to send
//! let mut run = Run::new(Limits::default(), protocol, call.origin());
//! let mut action = run.start(&mut thread);
//! loop {
//!     let input = match action {
//!         Action::Send => {
//!             let request = protocol::build_request(protocol, &call, &thread)?;
//!             // Send it; feed each line of the body to a ReplyParser, showing its events.
//!             match send_and_parse(request) {
//!                 Ok(turn) => Input::Reply(turn),
//!                 // HTTP error, cut off, parse error: clear the streaming buffer.
//!                 Err(e) => Input::Failed(e),
//!                 // The writer stopped it: `parser.abort()` if any bytes arrived,
//!                 // so what they saw is kept; else nothing to keep.
//!                 Stopped(Some(parser)) => Input::Reply(parser.abort()),
//!                 Stopped(None) => Input::Stop(vec![]),
//!             }
//!         }
//!         Action::Wait { secs } => match sleep_unless_stopped(secs) {
//!             true => Input::Retry,
//!             false => Input::Stop(vec![]),
//!         },
//!         Action::RunTools(calls) => match run_tools(calls) {
//!             Done(results) => Input::Results(results),
//!             // The writer stopped the run: hand over the results of the tools
//!             // that finished; only the others are answered as cancelled.
//!             Stopped(finished) => Input::Stop(finished),
//!         },
//!         Action::Done(outcome) => break outcome,       // save the thread, show the outcome
//!     };
//!     action = run.next(&mut thread, input);
//!     // Save the thread here too, so a crash loses at most one step.
//! }
//! ```
//!
//! While the run [`Run::is_active`], lock the composer: don't call
//! [`Thread::push_user`] (it would cancel the calls the tools are running for).
//!
//! A failed request ([`Input::Failed`]) adds nothing to the thread; whatever
//! streamed before the failure was never stored, so the UI must clear its
//! streaming buffer (text, thinking, tool-call progress) before the next
//! attempt. Usage the stream reported before failing is not recorded. A
//! retryable failure is retried after [`Action::Wait`], up to
//! [`Limits::max_retries`] times per request.
//!
//! On [`Outcome::Withdrawn`] (a refusal), offer the text back to the writer
//! to rephrase. After [`Outcome::Error`], [`Run::start`] is the one way to
//! send again: after a failed request the thread is as it was before it;
//! after [`AgentError::Mismatch`] the calls are still waiting, and `start`
//! answers those whose arguments didn't parse with their parse errors and the
//! rest as cancelled.
//!
//! To pick up a thread saved mid-run (after a crash, or opened from a file),
//! compare its tools with [`crate::file::check_tools`], then call
//! [`Run::start`] too: calls still waiting are answered as cancelled first.
//!
//! [`cancel`] is public too: it repairs a thread left with unanswered calls
//! ([`Thread::push_user`] calls it).

use crate::thread::{Origin, StopReason, Thread, ToolCall, ToolResult, Turn};
use crate::{AgentError, Protocol};

/// The result given to a call that never ran because the writer stopped the run.
pub const CANCELLED: &str = "Cancelled: the writer stopped the run before this tool ran.";

/// Notice stored with the results of a cancelled tool run.
pub const CANCELLED_NOTICE: &str = "Stopped before the tools ran.";

/// A reply's tool calls, sorted.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Triage {
    /// Calls to run.
    pub run: Vec<ToolCall>,
    /// Results for calls that must not run (their arguments didn't parse).
    pub answered: Vec<ToolResult>,
}

/// Splits calls into those to run and those answered at once with an error
/// (arguments that weren't valid JSON, e.g. cut off at the output limit).
pub(crate) fn triage(calls: Vec<ToolCall>) -> Triage {
    let mut t = Triage::default();
    for c in calls {
        match input_error_result(&c) {
            Some(r) => t.answered.push(r),
            None => t.run.push(c),
        }
    }
    t
}

/// The error result for a call whose arguments didn't parse, or `None` if it may run.
fn input_error_result(call: &ToolCall) -> Option<ToolResult> {
    let why = call.input_error.as_ref()?;
    Some(ToolResult {
        id: call.id.clone(),
        content: format!("The tool didn't run: {why}. Call it again with complete, valid JSON arguments."),
        is_error: true,
    })
}

/// Answers every call still waiting for a result with [`CANCELLED`], plus a
/// notice for the writer, right after the reply that made it (so a thread
/// left with calls stranded behind later messages, e.g. by a crash, becomes
/// sendable again). Returns how many calls it answered (0 if none were
/// waiting; the thread is then unchanged).
pub fn cancel(thread: &mut Thread) -> usize {
    let mut n = 0;
    while let Some((at, calls)) = thread.pending() {
        let results: Vec<ToolResult> =
            calls.iter().map(|c| ToolResult { id: c.id.clone(), content: CANCELLED.into(), is_error: true }).collect();
        n += results.len();
        thread.answer(at, results, Some(CANCELLED_NOTICE.into()));
    }
    n
}

/// Answers every call still waiting: the earliest waiting calls with the
/// `given` results for them (the first for each id; results for other calls
/// are dropped), the rest as cancelled (with [`CANCELLED_NOTICE`] when any
/// are). Calls stranded further back (a damaged thread) are cancelled too.
fn settle(thread: &mut Thread, given: Vec<ToolResult>) {
    if let Some((at, calls)) = thread.pending() {
        let ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
        let mut kept: Vec<ToolResult> = vec![];
        for r in given {
            if ids.contains(&r.id) && !kept.iter().any(|k| k.id == r.id) {
                kept.push(r);
            }
        }
        let cancelled: Vec<ToolResult> = ids
            .iter()
            .filter(|id| !kept.iter().any(|k| k.id == **id))
            .map(|id| ToolResult { id: id.clone(), content: CANCELLED.into(), is_error: true })
            .collect();
        let notice = (!cancelled.is_empty()).then(|| CANCELLED_NOTICE.to_string());
        kept.extend(cancelled);
        thread.answer(at, kept, notice);
    }
    cancel(thread);
}

/// The result given to calls left unrun when a run hits its tool round limit.
pub const ROUND_LIMIT: &str = "stopped: tool round limit";

/// Notice stored with the results of calls left unrun at the round limit.
pub const ROUND_LIMIT_NOTICE: &str = "Stopped: the agent reached its limit of tool rounds.";

/// Bounds on one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How many times the run may run tools. A reply asking for more is
    /// answered with [`ROUND_LIMIT`] results and the run ends
    /// ([`Outcome::RoundLimit`]).
    pub max_tool_rounds: usize,
    /// How many times one request is retried after a retryable failure
    /// ([`Action::Wait`]) before the run ends with the error.
    pub max_retries: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_tool_rounds: 25, max_retries: 3 }
    }
}

/// The longest wait between retries the run picks itself, in seconds (a
/// provider's `retry-after` is honoured up to [`MAX_RETRY_AFTER_SECS`]).
pub const MAX_BACKOFF_SECS: u64 = 30;

/// The longest `retry-after` a run waits for, in seconds. A provider asking
/// for longer ends the run with its error instead (the writer can retry
/// later themselves) rather than leaving it waiting indefinitely.
pub const MAX_RETRY_AFTER_SECS: u64 = 300;

/// What the shell reports to [`Run::next`].
// Built once per step and moved straight in: boxing the turn would only
// make the shell's code noisier.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A finished reply (or [`crate::ReplyParser::abort`] if the writer
    /// stopped it after bytes arrived).
    Reply(Turn),
    /// The results of the calls from [`Action::RunTools`], in any order.
    Results(Vec<ToolResult>),
    /// The request failed (HTTP error, connection cut, unparsable reply).
    /// Nothing is added to the thread.
    Failed(AgentError),
    /// The wait from [`Action::Wait`] is over: send again.
    Retry,
    /// The writer stopped the run, with the results of the tools that
    /// finished before it did (empty if none ran, or when stopping while a
    /// request is sent or waited on). Calls without a result are answered
    /// as cancelled.
    Stop(Vec<ToolResult>),
}

/// What the shell should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Build a request from the thread and send it.
    Send,
    /// A retryable failure: wait this long, then report [`Input::Retry`]
    /// (or [`Input::Stop`] if the writer stops meanwhile).
    Wait { secs: u64 },
    /// Run these calls and report [`Input::Results`] (or [`Input::Stop`]).
    RunTools(Vec<ToolCall>),
    /// The run is over.
    Done(Outcome),
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The model finished its reply (or a refusal left nothing to withdraw).
    Finished,
    /// The model refused; the writer's message was taken out of the thread
    /// and is offered back for rephrasing.
    Withdrawn(String),
    /// The writer stopped the run; calls without results were answered as cancelled.
    Stopped,
    /// Something failed. After a failed request (retries used up, or not
    /// retryable) the thread is unchanged and may be sent again.
    ///
    /// [`AgentError::Mismatch`] means the shell's [`Input::Results`] didn't
    /// answer the calls exactly (a bug in the shell): nothing was added, and
    /// the run is over like any other. [`Run::start`] answers the calls still
    /// waiting (those whose arguments didn't parse with their parse errors,
    /// which the run keeps, the rest as cancelled) and sends again.
    Error(AgentError),
    /// The run used up [`Limits::max_tool_rounds`].
    RoundLimit,
}

/// One run of the agent, as a state machine over [`Input`]s and [`Action`]s,
/// against one endpoint and model (switch between runs, not inside one).
/// Pure: the shell sends requests, sleeps, runs tools and saves the thread.
#[derive(Debug, Clone)]
pub struct Run {
    limits: Limits,
    protocol: Protocol,
    origin: Origin,
    rounds: usize,
    /// Retries of the current request so far.
    retries: u32,
    active: bool,
    /// Results already made for calls that must not run (bad arguments),
    /// sent with the results of the calls that ran.
    answered: Vec<ToolResult>,
}

impl Run {
    /// A run sending in `protocol` to `origin` ([`crate::Call::origin`]):
    /// replies are stored with it, and the thread is checked against it.
    pub fn new(limits: Limits, protocol: Protocol, origin: Origin) -> Self {
        Run { limits, protocol, origin, rounds: 0, retries: 0, active: false, answered: vec![] }
    }

    /// How many tool rounds the run has started.
    pub fn rounds(&self) -> usize {
        self.rounds
    }

    /// Whether the run is under way: it has asked for an action other than
    /// [`Action::Done`]. While it is, keep the composer locked (see
    /// [`Thread::push_user`]).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The first action, and the one way to send again (after an error, or
    /// for a thread saved mid-run). Calls still waiting for results are
    /// answered first: with the results this run already made for them (calls
    /// whose arguments didn't parse, kept across an [`AgentError::Mismatch`]),
    /// the rest as cancelled. Then
    /// [`Action::Send`] if the thread can be sent, else [`Outcome::Error`]
    /// with [`AgentError::NotReady`] saying why not (e.g. it ends on the
    /// assistant's finished reply: nothing to send).
    pub fn start(&mut self, thread: &mut Thread) -> Action {
        self.retries = 0;
        let stored = std::mem::take(&mut self.answered);
        settle(thread, stored);
        self.send(thread)
    }

    pub fn next(&mut self, thread: &mut Thread, input: Input) -> Action {
        match input {
            Input::Reply(turn) => {
                self.retries = 0;
                self.reply(thread, turn)
            }
            Input::Results(results) => {
                let mut all = self.answered.clone();
                all.extend(results);
                match thread.push_tool_results(all) {
                    Ok(()) => {
                        self.answered.clear();
                        self.active = true;
                        Action::Send
                    }
                    // The run ends: see `Outcome::Error`. The bad-argument
                    // results stay stored, so `start` answers those calls
                    // with their parse errors rather than as cancelled.
                    Err(e) => self.done(Outcome::Error(e)),
                }
            }
            Input::Failed(e) => self.failed(e),
            Input::Retry => self.send(thread),
            Input::Stop(results) => self.stop(thread, results),
        }
    }

    fn send(&mut self, thread: &Thread) -> Action {
        match thread.check_sendable(self.protocol, &self.origin) {
            Ok(()) => {
                self.active = true;
                Action::Send
            }
            Err(e) => self.done(Outcome::Error(e)),
        }
    }

    fn done(&mut self, outcome: Outcome) -> Action {
        self.active = false;
        Action::Done(outcome)
    }

    fn failed(&mut self, e: AgentError) -> Action {
        let AgentError::Api { retryable: true, retry_after, .. } = &e else {
            return self.done(Outcome::Error(e));
        };
        if self.retries >= self.limits.max_retries || retry_after.is_some_and(|s| s > MAX_RETRY_AFTER_SECS) {
            return self.done(Outcome::Error(e));
        }
        // 1, 2, 4… seconds, capped.
        let backoff = 1u64.checked_shl(self.retries).unwrap_or(u64::MAX).min(MAX_BACKOFF_SECS);
        self.retries += 1;
        Action::Wait { secs: retry_after.unwrap_or(backoff) }
    }

    fn stop(&mut self, thread: &mut Thread, results: Vec<ToolResult>) -> Action {
        let mut given = std::mem::take(&mut self.answered);
        given.extend(results);
        settle(thread, given);
        self.done(Outcome::Stopped)
    }

    fn reply(&mut self, thread: &mut Thread, turn: Turn) -> Action {
        let stop = turn.stop.clone();
        let pushed = thread.push_turn(turn, self.origin.clone());
        match stop {
            StopReason::Cancelled => return self.done(Outcome::Stopped),
            StopReason::Refusal => {
                return self.done(pushed.withdrawn.map_or(Outcome::Finished, Outcome::Withdrawn));
            }
            _ => {}
        }
        if pushed.calls.is_empty() {
            return self.done(Outcome::Finished);
        }
        if self.rounds >= self.limits.max_tool_rounds {
            let results = pushed
                .calls
                .iter()
                .map(|c| ToolResult { id: c.id.clone(), content: ROUND_LIMIT.into(), is_error: true })
                .collect();
            if let Some((at, _)) = thread.pending() {
                thread.answer(at, results, Some(ROUND_LIMIT_NOTICE.into()));
            }
            return self.done(Outcome::RoundLimit);
        }
        self.rounds += 1;
        let Triage { run, answered } = triage(pushed.calls);
        if run.is_empty() {
            return match thread.push_tool_results(answered) {
                Ok(()) => {
                    self.active = true;
                    Action::Send
                }
                Err(e) => self.done(Outcome::Error(e)),
            };
        }
        self.answered = answered;
        self.active = true;
        Action::RunTools(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn call(id: &str, error: Option<&str>) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "suggest_change".into(),
            input: if error.is_some() { Value::Null } else { json!({}) },
            input_error: error.map(String::from),
            raw_input: None,
            extra: None,
        }
    }

    #[test]
    fn triage_answers_invalid_calls_and_keeps_the_rest_to_run() {
        let tr = triage(vec![call("a", None), call("b", Some("cut off")), call("c", None)]);
        let run: Vec<_> = tr.run.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(run, ["a", "c"]);
        assert_eq!(tr.answered.len(), 1);
        assert_eq!(tr.answered[0].id, "b");
        assert!(tr.answered[0].is_error);
        assert!(tr.answered[0].content.contains("cut off"));
        assert_eq!(input_error_result(&call("x", None)), None);
        assert_eq!(triage(vec![]), Triage::default());
    }
}
