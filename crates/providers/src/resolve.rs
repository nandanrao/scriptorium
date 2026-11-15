//! From an account and a thread to the HTTP request for its next turn.
//!
//! [`resolve`] settles everything that depends only on the account and the
//! model (once per run: [`scriptorium_agent::turn::Run::new`] takes its
//! [`Resolved::protocol`] and [`Resolved::origin`]); [`build_request`] then
//! builds each round's request with the key and the thread.

use scriptorium_agent::protocol::{self, Call, Request, Thinking, ToolSpec};
use scriptorium_agent::{Effort, Origin, Protocol, Thread};

use crate::ProvidersError;
use crate::account::Account;
use crate::models::capabilities;

/// An account's settings for one model, with what the model would refuse
/// left out. Holds no key.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// The account's id (for errors and the thread file).
    pub account: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    /// Whether [`build_request`] must be given a key.
    pub needs_key: bool,
    pub max_tokens: Option<u64>,
    pub effort: Option<Effort>,
    pub thinking: Thinking,
    pub headers: Vec<(String, String)>,
}

/// Settles `account`'s settings for `model` (the thread's model; `None` or
/// empty for the account's default).
///
/// For a model in [`crate::models`]: effort is lowered to a level it accepts
/// (or dropped), the output limit is capped at its maximum, and adaptive
/// thinking (summarized, so the writer sees progress) is asked for on the
/// Anthropic protocol when it takes it. For any other model, effort and the
/// output limit are sent as the writer set them and thinking is left to the
/// model's default: the table only removes what a known model would refuse.
pub fn resolve(account: &Account, model: Option<&str>) -> Result<Resolved, ProvidersError> {
    let model = model
        .filter(|m| !m.trim().is_empty())
        .or(account.model.as_deref())
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| ProvidersError::NoModel { account: account.id.clone() })?
        .to_string();
    let protocol = account.provider.protocol();
    let caps = capabilities(&model);
    let (effort, max_tokens, thinking) = match caps {
        Some(c) => (
            account.effort.and_then(|e| c.effort(e)),
            account.max_tokens.map(|n| n.min(c.max_output)),
            if c.adaptive_thinking && protocol == Protocol::Anthropic { Thinking::Adaptive } else { Thinking::Default },
        ),
        None => (account.effort, account.max_tokens, Thinking::Default),
    };
    Ok(Resolved {
        account: account.id.clone(),
        protocol,
        base_url: account.provider.base_url().trim_end_matches('/').to_string(),
        model,
        needs_key: account.provider.needs_key(),
        max_tokens,
        effort,
        thinking,
        headers: headers(account),
    })
}

/// The provider's default headers, then the account's (which replace a
/// provider header of the same name).
fn headers(account: &Account) -> Vec<(String, String)> {
    let mut headers = account.provider.default_headers();
    for (name, value) in &account.extra_headers {
        headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        headers.push((name.clone(), value.clone()));
    }
    headers
}

impl Resolved {
    /// The origin replies are stored with, and a [`scriptorium_agent::turn::Run`] is fixed to.
    pub fn origin(&self) -> Origin {
        Origin::new(&self.base_url, &self.model)
    }

    /// The agent crate's [`Call`] for these settings. `system` and `tools`
    /// must stay byte-identical for the whole thread (see
    /// [`protocol::build_request`]).
    pub fn call<'a>(&'a self, api_key: &'a str, system: &'a str, tools: &'a [ToolSpec]) -> Call<'a> {
        Call {
            base_url: &self.base_url,
            api_key,
            model: &self.model,
            system,
            tools,
            max_tokens: self.max_tokens,
            effort: self.effort,
            thinking: self.thinking,
            extra_headers: &self.headers,
        }
    }

    fn key<'a>(&self, api_key: Option<&'a str>) -> Result<&'a str, ProvidersError> {
        let key = api_key.map(str::trim).unwrap_or("");
        if key.is_empty() && self.needs_key {
            return Err(ProvidersError::MissingKey { account: self.account.clone() });
        }
        Ok(key)
    }
}

/// The streaming request for `thread`'s next turn. `api_key` is the
/// account's key as the shell found it (surrounding whitespace is ignored);
/// `None` or empty is refused for providers that need one.
pub fn build_request(
    resolved: &Resolved,
    api_key: Option<&str>,
    system: &str,
    tools: &[ToolSpec],
    thread: &Thread,
) -> Result<Request, ProvidersError> {
    let key = resolved.key(api_key)?;
    Ok(protocol::build_request(resolved.protocol, &resolved.call(key, system, tools), thread)?)
}

/// The request listing `account`'s models (sends the key, no script text);
/// read the reply with [`protocol::parse_models`].
pub fn models_request(account: &Account, api_key: Option<&str>) -> Result<Request, ProvidersError> {
    let p = &account.provider;
    let key = api_key.map(str::trim).unwrap_or("");
    if key.is_empty() && p.needs_key() {
        return Err(ProvidersError::MissingKey { account: account.id.clone() });
    }
    let mut request = protocol::models_request(p.protocol(), p.base_url(), key);
    request.headers.extend(headers(account));
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Provider;
    use scriptorium_agent::{AgentError, Message};

    fn thread() -> Thread {
        let mut t = Thread::default();
        t.messages.push(Message::user("Tighten scene 3."));
        t
    }

    fn header<'a>(r: &'a Request, name: &str) -> Option<&'a str> {
        r.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    #[test]
    fn anthropic_request() {
        let mut a = Account::new("work", "Work", Provider::Anthropic);
        a.effort = Some(Effort::High);
        a.max_tokens = Some(500_000);
        let r = resolve(&a, None).unwrap();
        assert_eq!(r.model, "claude-opus-5-5");
        assert_eq!(r.max_tokens, Some(128_000), "capped at the model's maximum");
        assert_eq!(r.thinking, Thinking::Adaptive);
        assert_eq!(r.origin(), Origin::new("https://api.anthropic.com/v1", "claude-opus-5-5"));

        let req = build_request(&r, Some(" sk-ant-1\n"), "You help.", &[], &thread()).unwrap();
        assert_eq!(req.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(header(&req, "x-api-key"), Some("sk-ant-1"));
        assert_eq!(req.body["model"], "claude-opus-5-5");
        assert_eq!(req.body["max_tokens"], 128_000);
        assert_eq!(req.body["output_config"]["effort"], "high");
        assert_eq!(req.body["thinking"]["type"], "adaptive");
        assert_eq!(req.body["system"], "You help.");
    }

    #[test]
    fn the_threads_model_wins_and_known_models_lose_what_they_refuse() {
        let mut a = Account::new("work", "Work", Provider::Anthropic);
        a.effort = Some(Effort::XHigh);
        let r = resolve(&a, Some("claude-haiku-4-5")).unwrap();
        assert_eq!(r.model, "claude-haiku-4-5");
        assert_eq!((r.effort, r.thinking), (None, Thinking::Default));
        let req = build_request(&r, Some("k"), "", &[], &thread()).unwrap();
        assert!(req.body.get("output_config").is_none());
        assert!(req.body.get("thinking").is_none());

        assert_eq!(resolve(&a, Some("claude-sonnet-4-6")).unwrap().effort, Some(Effort::High));
        assert_eq!(resolve(&a, Some("")).unwrap().model, "claude-opus-5-5", "empty means the default");
    }

    #[test]
    fn unknown_models_get_what_the_writer_set() {
        let mut a = Account::new("o", "OpenAI", Provider::Openai);
        a.model = Some("gpt-9".into());
        a.effort = Some(Effort::Low);
        a.max_tokens = Some(4096);
        let r = resolve(&a, None).unwrap();
        assert_eq!((r.effort, r.max_tokens, r.thinking), (Some(Effort::Low), Some(4096), Thinking::Default));
        let req = build_request(&r, Some("sk-1"), "sys", &[], &thread()).unwrap();
        assert_eq!(req.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(header(&req, "authorization"), Some("Bearer sk-1"));
        assert_eq!(req.body["reasoning_effort"], "low");
        assert_eq!(req.body["max_completion_tokens"], 4096);
    }

    #[test]
    fn openrouter_headers() {
        let mut a = Account::new("or", "OpenRouter", Provider::Openrouter);
        a.model = Some("anthropic/claude-opus-5-5".into());
        a.extra_headers.insert("HTTP-Referer".into(), "https://example.org".into());
        let r = resolve(&a, None).unwrap();
        assert_eq!(r.thinking, Thinking::Default, "adaptive thinking is an Anthropic-protocol setting");
        let req = build_request(&r, Some("sk-or-1"), "", &[], &thread()).unwrap();
        assert_eq!(req.url, "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(header(&req, "X-Title"), Some("Scriptorium"));
        assert_eq!(header(&req, "HTTP-Referer"), Some("https://example.org"));

        // The account's header replaces the provider's.
        a.extra_headers.insert("x-title".into(), "Mine".into());
        let r = resolve(&a, None).unwrap();
        assert_eq!(r.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case("x-title")).count(), 1);
        assert_eq!(header(&build_request(&r, Some("k"), "", &[], &thread()).unwrap(), "x-title"), Some("Mine"));
    }

    #[test]
    fn local_server_needs_no_key() {
        let mut a = Account::new(
            "ollama",
            "Ollama",
            Provider::Custom { protocol: Protocol::OpenaiChat, base_url: "http://localhost:11434/v1/".into() },
        );
        a.model = Some("qwen3".into());
        let r = resolve(&a, None).unwrap();
        assert_eq!(r.base_url, "http://localhost:11434/v1");
        let req = build_request(&r, None, "", &[], &thread()).unwrap();
        assert_eq!(req.url, "http://localhost:11434/v1/chat/completions");
        assert_eq!(header(&req, "authorization"), None);
        assert_eq!(models_request(&a, None).unwrap().url, "http://localhost:11434/v1/models");
    }

    #[test]
    fn errors() {
        let mut a = Account::new("o", "OpenAI", Provider::Openai);
        assert_eq!(resolve(&a, None), Err(ProvidersError::NoModel { account: "o".into() }));
        a.model = Some("gpt-9".into());
        let r = resolve(&a, None).unwrap();
        for key in [None, Some(""), Some("  ")] {
            assert_eq!(
                build_request(&r, key, "", &[], &thread()),
                Err(ProvidersError::MissingKey { account: "o".into() })
            );
        }
        assert!(matches!(models_request(&a, None), Err(ProvidersError::MissingKey { .. })));
        assert!(matches!(
            build_request(&r, Some("k"), "", &[], &Thread::default()),
            Err(ProvidersError::Agent(AgentError::NotReady(_)))
        ));
    }

    #[test]
    fn models_request_carries_key_and_headers() {
        let a = Account::new("or", "OpenRouter", Provider::Openrouter);
        let req = models_request(&a, Some("sk-or-1")).unwrap();
        assert_eq!(req.url, "https://openrouter.ai/api/v1/models");
        assert_eq!(header(&req, "authorization"), Some("Bearer sk-or-1"));
        assert_eq!(header(&req, "X-Title"), Some("Scriptorium"));
    }
}
