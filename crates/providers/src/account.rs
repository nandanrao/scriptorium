//! The writer's provider accounts and the file they're kept in.
//!
//! An account holds settings only, never a key: keys live in the OS keyring
//! or an environment variable (the shell's job), so the accounts file can't
//! leak one. [`AccountsFile::check`] refuses extra headers that would carry
//! credentials for the same reason.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use scriptorium_agent::presets::{self, Preset};
use scriptorium_agent::{Effort, Protocol};

use crate::ProvidersError;
use crate::models::Price;

/// The format version this code writes.
pub const VERSION: u32 = 1;

/// Where an account's requests go.
///
/// Stored flattened into its [`Account`] as `"provider": "anthropic"`, or for
/// a custom endpoint `"provider": "custom", "protocol": "openai_chat",
/// "base_url": "http://localhost:11434/v1"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    Openai,
    Openrouter,
    /// Any other endpoint speaking one of the two protocols: a local server
    /// (Ollama, LM Studio), another OpenAI-compatible provider, or an
    /// Anthropic-compatible proxy.
    Custom {
        protocol: Protocol,
        base_url: String,
    },
}

impl Provider {
    /// The providers offered by name, in the order to list them.
    pub const PRESETS: [Provider; 3] = [Provider::Anthropic, Provider::Openai, Provider::Openrouter];

    /// The agent crate's preset for this provider (endpoint, key page,
    /// suggested model); `None` for a custom endpoint.
    pub fn preset(&self) -> Option<&'static Preset> {
        let id = match self {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::Openrouter => "openrouter",
            Provider::Custom { .. } => return None,
        };
        Some(presets::find(id).expect("the agent crate lists every preset provider"))
    }

    pub fn protocol(&self) -> Protocol {
        match self {
            Provider::Custom { protocol, .. } => *protocol,
            p => p.preset().expect("preset").protocol,
        }
    }

    /// The base URL, as stored (requests trim a trailing slash).
    pub fn base_url(&self) -> &str {
        match self {
            Provider::Custom { base_url, .. } => base_url,
            p => p.preset().expect("preset").base_url,
        }
    }

    /// For the writer: "Anthropic", "OpenRouter", "Custom (OpenAI-compatible)".
    pub fn name(&self) -> String {
        match self {
            Provider::Custom { protocol, .. } => format!("Custom ({})", protocol.name()),
            p => p.preset().expect("preset").name.to_string(),
        }
    }

    /// Whether requests need an API key. A custom endpoint may not (local
    /// servers usually take none), so its key is optional.
    pub fn needs_key(&self) -> bool {
        !matches!(self, Provider::Custom { .. })
    }

    /// Extra headers every request to this provider carries, before the
    /// account's own. OpenRouter shows the app's name on its dashboards.
    pub fn default_headers(&self) -> Vec<(String, String)> {
        match self {
            Provider::Openrouter => vec![("X-Title".into(), "Scriptorium".into())],
            _ => vec![],
        }
    }
}

/// One account: a provider, its default model and per-account settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Stable id, unique in the file; threads refer to their account by it
    /// ([`scriptorium_agent::file::ThreadFile::account`]).
    pub id: String,
    /// For the writer.
    pub name: String,
    #[serde(flatten)]
    pub provider: Provider,
    /// The default model for new threads (a thread keeps its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort to ask for; `None` for the model's default. Dropped
    /// for models known not to accept it ([`crate::resolve()`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Output limit per reply; `None` for the protocol's default. Lowered to
    /// a known model's maximum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Added to every request (after the provider's [`Provider::default_headers`]).
    /// Never credentials: [`AccountsFile::check`] refuses those.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
    /// Prices per model id, for models the built-in table doesn't know or
    /// prices that differ on this endpoint. They win over the built-in table.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prices: BTreeMap<String, Price>,
}

/// Header names that carry credentials or that the protocol sets itself.
const RESERVED_HEADERS: &[&str] = &["authorization", "x-api-key", "api-key", "anthropic-version", "content-type"];

impl Account {
    /// A new account with the provider's suggested model, if it has one.
    pub fn new(id: &str, name: &str, provider: Provider) -> Self {
        let model = provider.preset().and_then(|p| p.default_model).map(String::from);
        Account {
            id: id.to_string(),
            name: name.to_string(),
            provider,
            model,
            effort: None,
            max_tokens: None,
            extra_headers: BTreeMap::new(),
            prices: BTreeMap::new(),
        }
    }

    /// The environment variable the shell reads this account's key from
    /// when there is no keyring: `SCRIPTORIUM_<PROVIDER>_KEY` for a named
    /// provider (so one key serves every account on it), else
    /// `SCRIPTORIUM_<ACCOUNT ID>_KEY`. Characters other than ASCII letters
    /// and digits become `_`.
    pub fn key_env_var(&self) -> String {
        let name = self.provider.preset().map_or(self.id.as_str(), |p| p.id);
        let upper: String =
            name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
        format!("SCRIPTORIUM_{upper}_KEY")
    }

    fn check(&self) -> Result<(), String> {
        let id = &self.id;
        if id.trim().is_empty() {
            return Err("an account has an empty id".into());
        }
        if let Provider::Custom { base_url, .. } = &self.provider {
            let rest = base_url.strip_prefix("https://").or_else(|| base_url.strip_prefix("http://"));
            if rest.is_none_or(|r| r.trim_matches('/').is_empty()) {
                return Err(format!("account \"{id}\": the base URL must start with http:// or https://"));
            }
        }
        if self.model.as_deref().is_some_and(|m| m.trim().is_empty()) {
            return Err(format!("account \"{id}\": the model is empty"));
        }
        if self.max_tokens == Some(0) {
            return Err(format!("account \"{id}\": max_tokens must be above 0"));
        }
        for name in self.extra_headers.keys() {
            if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                return Err(format!(
                    "account \"{id}\": the header \"{name}\" can't be set here (keys belong in the keychain)"
                ));
            }
        }
        for (model, price) in &self.prices {
            if !price.is_valid() {
                return Err(format!("account \"{id}\": the price for \"{model}\" must be finite and not negative"));
            }
        }
        Ok(())
    }
}

/// The accounts config file (`accounts.json`; where it lives is the shell's
/// business). Unknown fields are ignored on reading and lost on writing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountsFile {
    pub version: u32,
    /// The account new threads use, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

impl Default for AccountsFile {
    fn default() -> Self {
        AccountsFile { version: VERSION, default: None, accounts: vec![] }
    }
}

impl AccountsFile {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("accounts always serialize")
    }

    /// Reads a file written by this or an earlier version and checks it. A
    /// newer version is refused rather than half-read (and then overwritten
    /// without its new data).
    pub fn from_json(s: &str) -> Result<Self, ProvidersError> {
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }
        let h: Header = serde_json::from_str(s).map_err(|e| ProvidersError::Config(e.to_string()))?;
        if h.version == 0 || h.version > VERSION {
            return Err(ProvidersError::Config(format!(
                "version {} is not supported (this version of Scriptorium reads up to {VERSION})",
                h.version
            )));
        }
        let file: AccountsFile = serde_json::from_str(s).map_err(|e| ProvidersError::Config(e.to_string()))?;
        file.check()?;
        Ok(file)
    }

    /// Ids are unique and not empty, the default names an account, custom
    /// base URLs are http(s), no credentials in extra headers, prices are sane.
    pub fn check(&self) -> Result<(), ProvidersError> {
        for (i, a) in self.accounts.iter().enumerate() {
            a.check().map_err(ProvidersError::Config)?;
            if self.accounts[..i].iter().any(|b| b.id == a.id) {
                return Err(ProvidersError::Config(format!("two accounts have the id \"{}\"", a.id)));
            }
        }
        if let Some(d) = &self.default
            && self.get(d).is_none()
        {
            return Err(ProvidersError::Config(format!("the default account \"{d}\" doesn't exist")));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// The default account, or the only one when there's just one.
    pub fn default_account(&self) -> Option<&Account> {
        match &self.default {
            Some(id) => self.get(id),
            None if self.accounts.len() == 1 => self.accounts.first(),
            None => None,
        }
    }

    /// Adds an account, or replaces the one with the same id. The file is
    /// unchanged if the result wouldn't pass [`AccountsFile::check`].
    pub fn upsert(&mut self, account: Account) -> Result<(), ProvidersError> {
        let mut next = self.clone();
        match next.accounts.iter_mut().find(|a| a.id == account.id) {
            Some(a) => *a = account,
            None => next.accounts.push(account),
        }
        next.check()?;
        *self = next;
        Ok(())
    }

    /// Removes an account (clearing the default if it was that one).
    /// Returns it, so the shell can delete its keyring entry.
    pub fn remove(&mut self, id: &str) -> Option<Account> {
        let i = self.accounts.iter().position(|a| a.id == id)?;
        if self.default.as_deref() == Some(id) {
            self.default = None;
        }
        Some(self.accounts.remove(i))
    }

    pub fn set_default(&mut self, id: &str) -> Result<(), ProvidersError> {
        if self.get(id).is_none() {
            return Err(ProvidersError::UnknownAccount(id.to_string()));
        }
        self.default = Some(id.to_string());
        Ok(())
    }

    /// An id for a new account based on `base` (e.g. the provider's id):
    /// `base`, else `base-2`, `base-3`…
    pub fn unused_id(&self, base: &str) -> String {
        if self.get(base).is_none() {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}-{n}")).find(|id| self.get(id).is_none()).expect("some id is free")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> Account {
        Account::new(
            "ollama",
            "Ollama",
            Provider::Custom { protocol: Protocol::OpenaiChat, base_url: "http://localhost:11434/v1".into() },
        )
    }

    #[test]
    fn presets_come_from_the_agent_crate() {
        assert_eq!(Provider::Anthropic.base_url(), "https://api.anthropic.com/v1");
        assert_eq!(Provider::Anthropic.protocol(), Protocol::Anthropic);
        assert_eq!(Provider::Openai.protocol(), Protocol::OpenaiChat);
        assert_eq!(Provider::Openrouter.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(Provider::Openrouter.name(), "OpenRouter");
        assert_eq!(local().provider.name(), "Custom (OpenAI-compatible)");
        assert!(Provider::Openai.needs_key());
        assert!(!local().provider.needs_key());
    }

    #[test]
    fn new_account_takes_the_suggested_model() {
        assert_eq!(Account::new("a", "A", Provider::Anthropic).model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(Account::new("o", "O", Provider::Openai).model, None);
    }

    #[test]
    fn env_var_names() {
        assert_eq!(Account::new("work", "Work", Provider::Anthropic).key_env_var(), "SCRIPTORIUM_ANTHROPIC_KEY");
        assert_eq!(Account::new("x", "X", Provider::Openrouter).key_env_var(), "SCRIPTORIUM_OPENROUTER_KEY");
        let mut l = local();
        l.id = "lm-studio.2".into();
        assert_eq!(l.key_env_var(), "SCRIPTORIUM_LM_STUDIO_2_KEY");
    }

    #[test]
    fn file_format_round_trips_and_is_flat() {
        let mut file = AccountsFile::default();
        let mut a = Account::new("anthropic", "Anthropic", Provider::Anthropic);
        a.effort = Some(Effort::XHigh);
        a.max_tokens = Some(32_000);
        file.upsert(a).unwrap();
        let mut l = local();
        l.prices.insert(
            "qwen3".into(),
            Price { input: 0.0, output: 0.0, cache_read: None, cache_write: None, above: None },
        );
        file.upsert(l).unwrap();
        file.set_default("anthropic").unwrap();

        let json = file.to_json();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["default"], "anthropic");
        assert_eq!(v["accounts"][0]["provider"], "anthropic");
        assert_eq!(v["accounts"][0]["effort"], "xhigh");
        assert!(v["accounts"][0].get("base_url").is_none());
        assert_eq!(v["accounts"][1]["provider"], "custom");
        assert_eq!(v["accounts"][1]["protocol"], "openai_chat");
        assert_eq!(v["accounts"][1]["base_url"], "http://localhost:11434/v1");
        assert!(v["accounts"][1].get("extra_headers").is_none(), "empty maps are left out");
        assert_eq!(AccountsFile::from_json(&json).unwrap(), file);
    }

    #[test]
    fn reads_a_hand_written_file() {
        let json = r#"{"version": 1, "accounts": [
            {"id": "or", "name": "OpenRouter", "provider": "openrouter", "model": "anthropic/claude-opus-5-5",
             "extra_headers": {"HTTP-Referer": "https://example.org"}, "future_field": true}
        ]}"#;
        let file = AccountsFile::from_json(json).unwrap();
        assert_eq!(file.accounts[0].provider, Provider::Openrouter);
        assert_eq!(file.default_account().unwrap().id, "or", "the only account is the default");
    }

    #[test]
    fn refuses_bad_files() {
        let bad = |json: &str| match AccountsFile::from_json(json) {
            Err(ProvidersError::Config(m)) => m,
            other => panic!("expected a config error for {json}, got {other:?}"),
        };
        assert!(bad(r#"{"version": 2, "accounts": []}"#).contains("version 2"));
        assert!(bad(r#"{"version": 0}"#).contains("version 0"));
        assert!(bad("not json").contains("expected"));
        bad(r#"{"version": 1, "accounts": [{"id": "g", "name": "G", "provider": "gemini"}]}"#);
        bad(
            r#"{"version": 1, "accounts": [{"id": "c", "name": "C", "provider": "custom", "protocol": "openai_chat"}]}"#,
        );
        assert!(
            bad(r#"{"version": 1, "accounts": [{"id": "c", "name": "C", "provider": "custom", "protocol": "anthropic", "base_url": "localhost:8080"}]}"#)
                .contains("http")
        );
        assert!(bad(r#"{"version": 1, "accounts": [{"id": "a", "name": "A", "provider": "anthropic"}, {"id": "a", "name": "B", "provider": "openai"}]}"#).contains("two accounts"));
        assert!(bad(r#"{"version": 1, "default": "x", "accounts": []}"#).contains("default"));
        assert!(
            bad(r#"{"version": 1, "accounts": [{"id": "a", "name": "A", "provider": "openai", "extra_headers": {"Authorization": "Bearer sk-1"}}]}"#)
                .contains("keychain")
        );
        bad(r#"{"version": 1, "accounts": [{"id": " ", "name": "A", "provider": "openai"}]}"#);
        bad(r#"{"version": 1, "accounts": [{"id": "a", "name": "A", "provider": "openai", "max_tokens": 0}]}"#);
        bad(
            r#"{"version": 1, "accounts": [{"id": "a", "name": "A", "provider": "openai", "prices": {"m": {"input": -1, "output": 1}}}]}"#,
        );
    }

    #[test]
    fn editing_accounts() {
        let mut file = AccountsFile::default();
        file.upsert(Account::new("anthropic", "Anthropic", Provider::Anthropic)).unwrap();
        assert_eq!(file.unused_id("anthropic"), "anthropic-2");
        assert_eq!(file.unused_id("openai"), "openai");

        // A bad edit leaves the file as it was.
        let mut bad = local();
        bad.provider = Provider::Custom { protocol: Protocol::OpenaiChat, base_url: "ftp://x".into() };
        assert!(file.upsert(bad).is_err());
        assert_eq!(file.accounts.len(), 1);

        let mut renamed = Account::new("anthropic", "Work", Provider::Anthropic);
        renamed.model = Some("claude-sonnet-5-5".into());
        file.upsert(renamed).unwrap();
        assert_eq!(file.accounts.len(), 1);
        assert_eq!(file.accounts[0].name, "Work");

        file.upsert(local()).unwrap();
        assert_eq!(file.default_account(), None, "two accounts and no default");
        assert!(matches!(file.set_default("nope"), Err(ProvidersError::UnknownAccount(_))));
        file.set_default("anthropic").unwrap();
        assert_eq!(file.remove("anthropic").unwrap().name, "Work");
        assert_eq!(file.default, None);
        assert_eq!(file.remove("anthropic"), None);
        assert_eq!(file.default_account().unwrap().id, "ollama");
    }
}
