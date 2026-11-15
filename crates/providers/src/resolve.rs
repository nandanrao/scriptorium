//! From an account and a thread to the HTTP request for its next turn.
//!
//! [`resolve()`] settles everything that depends only on the account and the
//! model (once per run: [`scriptorium_agent::turn::Run::new`] takes its
//! [`Resolved::protocol`] and [`Resolved::origin`]); [`build_request`] then
//! builds each round's request with the key and the thread.

use scriptorium_agent::protocol::{self, Call, Request, Thinking, ToolSpec};
use scriptorium_agent::{Effort, Origin, Protocol, Thread};

use crate::ProvidersError;
use crate::account::{Account, header_problem};
use crate::models::{ChatTools, capabilities};

/// The output limit for a known model when the account sets none. Streaming,
/// so a large limit doesn't risk a timeout; long review passes need room.
pub const DEFAULT_MAX_TOKENS: u64 = 64_000;

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
    /// Send `reasoning_effort: "none"` in place of [`Resolved::effort`]: an
    /// OpenAI model that calls tools on Chat Completions only without
    /// reasoning ([`ChatTools::WithoutReasoning`]).
    pub reasoning_off: bool,
    pub thinking: Thinking,
    /// Extra headers: the provider's, then the account's valid,
    /// non-reserved ones.
    pub headers: Vec<(String, String)>,
}

/// Settles `account`'s settings for `model` (the thread's model; `None` or
/// empty for the account's default).
///
/// For a model in [`crate::models`]: effort is lowered to a level it accepts
/// (or dropped), the output limit is the account's or [`DEFAULT_MAX_TOKENS`],
/// capped at the model's maximum, and adaptive thinking (summarized, so the
/// writer sees progress) is asked for on the Anthropic protocol when it takes
/// it. For any other model, effort and the output limit are sent as the
/// writer set them (no limit set: the protocol's default) and thinking is
/// left to the model's default: the table only removes what a known model
/// would refuse. On the OpenAI Chat Completions protocol, a model that takes
/// tools only without reasoning gets `reasoning_effort: "none"` whatever
/// effort the account sets.
pub fn resolve(account: &Account, model: Option<&str>) -> Result<Resolved, ProvidersError> {
    let model = model
        .filter(|m| !m.trim().is_empty())
        .or(account.model.as_deref())
        .filter(|m| !m.trim().is_empty())
        // A named provider's account without a model uses the provider's
        // default; a custom endpoint has none to fall back on.
        .or(account.provider.default_model())
        .ok_or_else(|| ProvidersError::NoModel { account: account.id.clone() })?
        .to_string();
    let protocol = account.provider.protocol();
    let caps = capabilities(&model);
    let (effort, max_tokens, thinking) = match caps {
        Some(c) => (
            account.effort.and_then(|e| c.effort(e)),
            Some(account.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS).min(c.max_output)),
            if c.adaptive_thinking && protocol == Protocol::Anthropic { Thinking::Adaptive } else { Thinking::Default },
        ),
        None => (account.effort, account.max_tokens, Thinking::Default),
    };
    let reasoning_off =
        protocol == Protocol::OpenaiChat && caps.is_some_and(|c| c.chat_tools == ChatTools::WithoutReasoning);
    let effort = if reasoning_off { None } else { effort };
    Ok(Resolved {
        account: account.id.clone(),
        protocol,
        base_url: account.provider.base_url().trim_end_matches('/').to_string(),
        model,
        needs_key: account.provider.needs_key(),
        max_tokens,
        effort,
        reasoning_off,
        thinking,
        headers: headers(account),
    })
}

/// The provider's default headers, then the account's (which replace a
/// provider header of the same name). Headers [`crate::AccountsFile::check`]
/// would refuse are left out here too, so an account that was never checked
/// can't send a credential or a second copy of a header the protocol sets.
fn headers(account: &Account) -> Vec<(String, String)> {
    let mut headers = account.provider.default_headers();
    for (name, value) in &account.extra_headers {
        let name = name.trim();
        if header_problem(name, value).is_some() {
            continue;
        }
        headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        headers.push((name.to_string(), value.clone()));
    }
    headers
}

impl Resolved {
    /// The origin replies are stored with, and a [`scriptorium_agent::turn::Run`] is fixed to.
    pub fn origin(&self) -> Origin {
        Origin::new(&self.base_url, &self.model)
    }

    fn call<'a>(&'a self, api_key: &'a str, system: &'a str, tools: &'a [ToolSpec]) -> Call<'a> {
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

/// The streaming request for `thread`'s next turn: the one way to build it.
/// `api_key` is the account's key as the shell found it (surrounding
/// whitespace is ignored); `None` or empty is refused for providers that
/// need one. `system` and `tools` must stay byte-identical for the whole
/// thread (see [`protocol::build_request`]). HTTP errors in reply are read
/// with [`protocol::api_error`].
pub fn build_request(
    resolved: &Resolved,
    api_key: Option<&str>,
    system: &str,
    tools: &[ToolSpec],
    thread: &Thread,
) -> Result<Request, ProvidersError> {
    let key = resolved.key(api_key)?;
    let mut request = protocol::build_request(resolved.protocol, &resolved.call(key, system, tools), thread)?;
    if resolved.reasoning_off {
        request.body["reasoning_effort"] = "none".into();
    }
    Ok(request)
}

/// The accounts sheet's "Check" request: lists `account`'s models. It is
/// cheap (no tokens) and sends the key but no script text, so it tells the
/// writer whether the key and address work, and gives the picker the
/// endpoint's own model list. Read the reply with [`protocol::parse_models`]
/// (or [`protocol::api_error`] for a failure).
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
        a.max_tokens = None;
        assert_eq!(resolve(&a, None).unwrap().max_tokens, Some(64_000));
        assert_eq!(resolve(&a, Some("claude-haiku-4-5")).unwrap().max_tokens, Some(64_000));
        a.max_tokens = Some(500_000);
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
        a.max_tokens = None;
        assert_eq!(resolve(&a, None).unwrap().max_tokens, None, "unknown model: the protocol's default");
    }

    #[test]
    fn openai_models_that_need_no_reasoning_for_tools_get_none() {
        let mut a = Account::new("o", "OpenAI", Provider::Openai);
        for effort in [None, Some(Effort::Low), Some(Effort::Max)] {
            a.effort = effort;
            for model in ["gpt-6-sol", "gpt-5.6-terra", "gpt-5.4-mini-2026-03-17"] {
                let r = resolve(&a, Some(model)).unwrap();
                assert!(r.reasoning_off && r.effort.is_none(), "{model}");
                let req = build_request(&r, Some("k"), "", &[], &thread()).unwrap();
                assert_eq!(req.body["reasoning_effort"], "none", "{model} {effort:?}");
                assert_eq!(req.body["max_completion_tokens"], DEFAULT_MAX_TOKENS, "a known model");
            }
        }
        // GPT-5.2 takes tools at any effort.
        a.effort = Some(Effort::Medium);
        let r = resolve(&a, Some("gpt-5.2")).unwrap();
        assert!(!r.reasoning_off);
        assert_eq!(build_request(&r, Some("k"), "", &[], &thread()).unwrap().body["reasoning_effort"], "medium");
        // A tool-less model goes out as written; the account warns instead.
        let r = resolve(&a, Some("gpt-6.1-sol")).unwrap();
        assert_eq!(build_request(&r, Some("k"), "", &[], &thread()).unwrap().body["reasoning_effort"], "medium");
        // Only on the OpenAI protocol: OpenRouter's ids aren't OpenAI's.
        let or = Account::new("or", "OpenRouter", Provider::Openrouter);
        assert!(!resolve(&or, Some("openai/gpt-6-sol")).unwrap().reasoning_off);
        let anthropic = Account::new(
            "c",
            "Proxy",
            Provider::Custom { protocol: Protocol::Anthropic, base_url: "https://proxy.example/v1".into() },
        );
        assert!(!resolve(&anthropic, Some("gpt-6-sol")).unwrap().reasoning_off);
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
        a.max_tokens = Some(2048);
        a.effort = Some(Effort::Max);
        let r = resolve(&a, None).unwrap();
        assert_eq!(r.base_url, "http://localhost:11434/v1");
        let req = build_request(&r, None, "", &[], &thread()).unwrap();
        assert_eq!(req.url, "http://localhost:11434/v1/chat/completions");
        assert_eq!(header(&req, "authorization"), None);
        assert_eq!(req.body["max_tokens"], 2048, "not OpenAI itself: the widely known name");
        assert_eq!(req.body["reasoning_effort"], "high", "OpenAI's highest level");
        assert_eq!(models_request(&a, None).unwrap().url, "http://localhost:11434/v1/models");
    }

    #[test]
    fn errors() {
        let mut a = Account::new("o", "OpenAI", Provider::Openai);
        a.model = None;
        // A named provider falls back to its default model; a custom one can't.
        assert_eq!(resolve(&a, None).unwrap().model, Provider::Openai.default_model().unwrap());
        let mut c = Account::new(
            "c",
            "Local",
            Provider::Custom { protocol: Protocol::OpenaiChat, base_url: "http://localhost:1/v1".into() },
        );
        c.model = None;
        assert_eq!(resolve(&c, None), Err(ProvidersError::NoModel { account: "c".into() }));
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

    #[test]
    fn openrouter_ids_find_the_table() {
        let mut a = Account::new("or", "OpenRouter", Provider::Openrouter);
        a.model = Some("anthropic/claude-sonnet-4.6:thinking".into());
        a.effort = Some(Effort::XHigh);
        let r = resolve(&a, None).unwrap();
        assert_eq!(r.effort, Some(Effort::High), "Sonnet 4.6 has no xhigh");
        assert_eq!(r.max_tokens, Some(64_000));
        assert_eq!(r.model, "anthropic/claude-sonnet-4.6:thinking", "sent as written");
    }

    #[test]
    fn unchecked_accounts_cant_send_reserved_headers() {
        let mut a = Account::new("o", "OpenAI", Provider::Openai);
        for (name, value) in [
            ("Authorization", "Bearer stolen"),
            (" authorization ", "Bearer stolen"),
            ("Content-Type", "text/plain"),
            ("Cookie", "s=1"),
            ("X-Bad Name", "x"),
            ("X-Smuggle", "a\r\nAuthorization: Bearer stolen"),
            (" X-Fine ", "yes"),
        ] {
            a.extra_headers.insert(name.into(), value.into());
        }
        let req = build_request(&resolve(&a, None).unwrap(), Some("sk-1"), "", &[], &thread()).unwrap();
        let auth: Vec<_> = req.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case("authorization")).collect();
        assert_eq!(auth, [&("authorization".to_string(), "Bearer sk-1".to_string())]);
        assert_eq!(req.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case("content-type")).count(), 1);
        assert_eq!(header(&req, "cookie"), None);
        assert!(req.headers.iter().all(|(n, v)| !n.contains(' ') && !v.contains('\n')));
        assert_eq!(header(&req, "X-Fine"), Some("yes"));
        let m = models_request(&a, Some("sk-1")).unwrap();
        assert_eq!(m.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case("authorization")).count(), 1);
    }
}
