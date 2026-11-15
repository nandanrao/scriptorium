//! The writer's provider accounts and the file they're kept in.
//!
//! An account holds settings only, never a key: keys live in the OS keyring
//! or an environment variable (the shell's job), so the accounts file can't
//! leak one. [`AccountsFile::check`] refuses extra headers that would carry
//! credentials for the same reason, and requests leave such headers out
//! even for an account that was never checked.
//!
//! The file is read leniently, so an older Scriptorium doesn't destroy what
//! a newer one wrote: an account for a provider this version doesn't know is
//! kept as written (hidden, and written back after the others), and fields
//! it doesn't know are kept on the file and on each account.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use scriptorium_agent::{Effort, Protocol};

use crate::ProvidersError;
use crate::models::Price;
use crate::presets::{self, Preset};
use crate::resolve::{Resolved, resolve};

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

    fn kind(&self) -> Kind<'_> {
        match self {
            Provider::Anthropic => Kind::Named(&presets::ANTHROPIC),
            Provider::Openai => Kind::Named(&presets::OPENAI),
            Provider::Openrouter => Kind::Named(&presets::OPENROUTER),
            Provider::Custom { protocol, base_url } => Kind::Custom(*protocol, base_url),
        }
    }

    /// The preset for a named provider; `None` for a custom endpoint.
    pub fn preset(&self) -> Option<&'static Preset> {
        match self.kind() {
            Kind::Named(p) => Some(p),
            Kind::Custom(..) => None,
        }
    }

    /// The id stored in the accounts file: `"anthropic"`, `"openai"`,
    /// `"openrouter"` or `"custom"`.
    pub fn id(&self) -> &'static str {
        match self.preset() {
            Some(p) => p.id,
            None => "custom",
        }
    }

    /// The named provider with this id (as [`Provider::id`] gives it,
    /// ignoring case and surrounding space). `None` for `"custom"`, which
    /// also needs a protocol and base URL, and for unknown ids.
    pub fn from_id(id: &str) -> Option<Provider> {
        let id = id.trim();
        Provider::PRESETS.into_iter().find(|p| p.id().eq_ignore_ascii_case(id))
    }

    pub fn protocol(&self) -> Protocol {
        match self.kind() {
            Kind::Named(p) => p.protocol,
            Kind::Custom(protocol, _) => protocol,
        }
    }

    /// The base URL, as stored (requests trim a trailing slash).
    pub fn base_url(&self) -> &str {
        match self.kind() {
            Kind::Named(p) => p.base_url,
            Kind::Custom(_, base_url) => base_url,
        }
    }

    /// For the writer: "Anthropic", "OpenRouter", "Custom (OpenAI-compatible)".
    pub fn name(&self) -> String {
        match self.kind() {
            Kind::Named(p) => p.name.to_string(),
            Kind::Custom(protocol, _) => format!("Custom ({})", protocol.name()),
        }
    }

    /// Suggested models for the picker, best first (empty for a custom endpoint).
    pub fn models(&self) -> &'static [&'static str] {
        self.preset().map_or(&[], |p| p.models)
    }

    /// The model new accounts start with, for a named provider.
    pub fn default_model(&self) -> Option<&'static str> {
        self.preset().map(|p| p.default_model)
    }

    /// Where the writer gets a key, for a named provider.
    pub fn keys_url(&self) -> Option<&'static str> {
        self.preset().map(|p| p.keys_url)
    }

    /// Whether requests need an API key. A custom endpoint may not (local
    /// servers usually take none), so its key is optional.
    pub fn needs_key(&self) -> bool {
        self.preset().is_some()
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

enum Kind<'a> {
    Named(&'static Preset),
    Custom(Protocol, &'a str),
}

/// One account: a provider, its default model and per-account settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Stable id, unique in the file; threads refer to their account by it
    /// ([`scriptorium_agent::file::ThreadFile::account`]).
    pub id: String,
    /// For the writer. Empty or missing in the file means the id.
    #[serde(default)]
    pub name: String,
    #[serde(flatten)]
    pub provider: Provider,
    /// The default model for new threads (a thread keeps its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort to ask for; `None` for the model's default. Lowered
    /// for models known not to accept it ([`crate::resolve()`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Output limit per reply; `None` for the default (see [`crate::resolve()`]).
    /// Lowered to a known model's maximum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Added to every request (after the provider's [`Provider::default_headers`]).
    /// Never credentials: [`AccountsFile::check`] refuses those, and requests
    /// leave them out.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
    /// Prices per model id, in US dollars per million tokens ([`Price`]),
    /// for models the built-in table doesn't know or prices that differ on
    /// this endpoint. They win over the built-in table.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prices: BTreeMap<String, Price>,
    /// Fields this version doesn't know, kept so they're written back.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Header names an account may not set: credentials, and what the protocol
/// or the HTTP client sets itself.
const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "openai-api-key",
    "anthropic-version",
    "content-type",
    "content-length",
    "transfer-encoding",
    "host",
];

/// An HTTP token (RFC 9110 `tchar`s): what a header name may be made of.
fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Why an extra header can't be sent, if it can't: not a valid name,
/// reserved, or a value with control characters (which could smuggle in
/// another header).
pub(crate) fn header_problem(name: &str, value: &str) -> Option<String> {
    if !is_token(name) {
        return Some(format!("\"{name}\" isn't a valid header name"));
    }
    if RESERVED_HEADERS.iter().any(|r| r.eq_ignore_ascii_case(name)) {
        return Some(format!("the header \"{name}\" can't be set here (keys belong in the keychain)"));
    }
    if value.chars().any(|c| c.is_control() && c != '\t') {
        return Some(format!("the header \"{name}\" has a control character in its value"));
    }
    None
}

/// `SCRIPTORIUM_<NAME>_KEY`, with characters other than ASCII letters and
/// digits turned into `_`.
fn env_var(name: &str) -> String {
    let upper: String =
        name.trim().chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
    format!("SCRIPTORIUM_{upper}_KEY")
}

/// Whether a URL's host is this machine or a private network, where plain
/// `http://` is normal.
fn is_local(base_url: &str) -> bool {
    let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    }
    .to_ascii_lowercase();
    let octets: Vec<u8> = host.split('.').filter_map(|o| o.parse().ok()).collect();
    let ipv4 = octets.len() == 4 && host.split('.').count() == 4;
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host == "::1"
        || (ipv4
            && (octets[0] == 127
                || octets[0] == 10
                || (octets[0] == 192 && octets[1] == 168)
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || octets == [0, 0, 0, 0]))
}

impl Account {
    /// A new account with the provider's default model, if it has one.
    pub fn new(id: &str, name: &str, provider: Provider) -> Self {
        Account {
            id: id.to_string(),
            name: name.to_string(),
            model: provider.default_model().map(String::from),
            provider,
            effort: None,
            max_tokens: None,
            extra_headers: BTreeMap::new(),
            prices: BTreeMap::new(),
            extra: Map::new(),
        }
    }

    /// The environment variables the shell reads this account's key from
    /// when there is no keyring, in lookup order: `SCRIPTORIUM_<ACCOUNT
    /// ID>_KEY`, then for a named provider `SCRIPTORIUM_<PROVIDER>_KEY` (so
    /// one key can serve every account on it). Characters other than ASCII
    /// letters and digits become `_`.
    pub fn key_env_vars(&self) -> Vec<String> {
        let mut vars = vec![env_var(&self.id)];
        if let Some(p) = self.provider.preset() {
            let shared = env_var(p.id);
            if shared != vars[0] {
                vars.push(shared);
            }
        }
        vars
    }

    /// Things worth telling the writer that don't stop the account working:
    /// a key sent over plain `http://` to another machine
    /// ([`Account::insecure_http`]), or a model that can't call tools here
    /// ([`Account::refuses_tools`]).
    pub fn warnings(&self) -> Vec<String> {
        let mut out = vec![];
        if self.insecure_http() {
            out.push(format!(
                "account \"{}\": {} is plain http to another machine; the key and the script text travel unencrypted",
                self.id,
                self.provider.base_url()
            ));
        }
        if let Some(model) = self.refuses_tools() {
            out.push(format!(
                "account \"{}\": {model} can't call tools over OpenAI's Chat Completions (only its Responses API), so it can't read or edit the script",
                self.id
            ));
        }
        out
    }

    /// Whether the key goes over plain `http://` to another machine.
    pub fn insecure_http(&self) -> bool {
        matches!(&self.provider, Provider::Custom { base_url, .. }
            if base_url.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) && !is_local(base_url))
    }

    /// The account's model, if the table knows it can't call tools on this
    /// account's protocol ([`crate::ChatTools::No`]).
    pub fn refuses_tools(&self) -> Option<&str> {
        let model = self.model.as_deref().filter(|m| !m.trim().is_empty()).or(self.provider.default_model())?;
        let no = self.provider.protocol() == Protocol::OpenaiChat
            && crate::models::capabilities(model).is_some_and(|c| c.chat_tools == crate::ChatTools::No);
        no.then_some(model)
    }

    fn check(&self) -> Result<(), String> {
        let id = &self.id;
        if id.trim().is_empty() {
            return Err("an account has an empty id".into());
        }
        if let Provider::Custom { base_url, .. } = &self.provider {
            let lower = base_url.to_ascii_lowercase();
            let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"));
            if rest.is_none_or(|r| r.trim_matches('/').is_empty()) {
                return Err(format!("account \"{id}\": the base URL must start with http:// or https://"));
            }
            if base_url.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(format!("account \"{id}\": the base URL has spaces in it"));
            }
        }
        if self.model.as_deref().is_some_and(|m| m.trim().is_empty()) {
            return Err(format!("account \"{id}\": the model is empty"));
        }
        if self.max_tokens == Some(0) {
            return Err(format!("account \"{id}\": max_tokens must be above 0"));
        }
        let names: Vec<&String> = self.extra_headers.keys().collect();
        for (i, (name, value)) in self.extra_headers.iter().enumerate() {
            if let Some(why) = header_problem(name, value) {
                return Err(format!("account \"{id}\": {why}"));
            }
            if names[..i].iter().any(|n| n.eq_ignore_ascii_case(name)) {
                return Err(format!("account \"{id}\": the header \"{name}\" is set twice"));
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
/// business).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawFile", into = "RawFile")]
pub struct AccountsFile {
    pub version: u32,
    /// The account new threads use, by id.
    pub default: Option<String>,
    /// The accounts this version can use.
    pub accounts: Vec<Account>,
    /// Accounts for providers this version doesn't know, as written.
    hidden: Vec<Value>,
    /// File fields this version doesn't know.
    extra: Map<String, Value>,
}

/// The file as JSON, before accounts are told apart.
#[derive(Serialize, Deserialize)]
struct RawFile {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    #[serde(default)]
    accounts: Vec<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl TryFrom<RawFile> for AccountsFile {
    type Error = String;

    fn try_from(raw: RawFile) -> Result<Self, String> {
        let mut accounts = vec![];
        let mut hidden = vec![];
        for v in raw.accounts {
            let provider = v.get("provider").and_then(Value::as_str);
            match provider {
                None => return Err("an account has no provider".into()),
                Some(p) if p != "custom" && Provider::from_id(p).is_none() => hidden.push(v),
                Some(_) => {
                    let mut a: Account = serde_json::from_value(v).map_err(|e| e.to_string())?;
                    // The flattened provider's fields also land in `extra`.
                    for k in ["provider", "protocol", "base_url"] {
                        a.extra.remove(k);
                    }
                    if a.name.trim().is_empty() {
                        a.name = a.id.clone();
                    }
                    accounts.push(a);
                }
            }
        }
        Ok(AccountsFile { version: raw.version, default: raw.default, accounts, hidden, extra: raw.extra })
    }
}

impl From<AccountsFile> for RawFile {
    fn from(f: AccountsFile) -> Self {
        let mut accounts: Vec<Value> =
            f.accounts.iter().map(|a| serde_json::to_value(a).expect("accounts always serialize")).collect();
        accounts.extend(f.hidden);
        RawFile { version: f.version, default: f.default, accounts, extra: f.extra }
    }
}

impl Default for AccountsFile {
    fn default() -> Self {
        AccountsFile { version: VERSION, default: None, accounts: vec![], hidden: vec![], extra: Map::new() }
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

    fn hidden_ids(&self) -> impl Iterator<Item = &str> {
        self.hidden.iter().filter_map(|v| v.get("id").and_then(Value::as_str))
    }

    fn id_taken(&self, id: &str) -> bool {
        self.get(id).is_some() || self.hidden_ids().any(|h| h == id)
    }

    /// Ids are unique and not empty, no two accounts read their key from the
    /// same environment variable and no id is another provider's name, the
    /// default names an account, custom base URLs are http(s), extra headers
    /// are valid and carry no credentials, prices are sane.
    pub fn check(&self) -> Result<(), ProvidersError> {
        let err = |m: String| Err(ProvidersError::Config(m));
        for (i, a) in self.accounts.iter().enumerate() {
            a.check().map_err(ProvidersError::Config)?;
            let earlier = &self.accounts[..i];
            if earlier.iter().any(|b| b.id == a.id) || self.hidden_ids().any(|h| h == a.id) {
                return err(format!("two accounts have the id \"{}\"", a.id));
            }
            let var = env_var(&a.id);
            if let Some(b) = earlier.iter().find(|b| env_var(&b.id) == var) {
                return err(format!("the accounts \"{}\" and \"{}\" would both read their key from {var}", b.id, a.id));
            }
            if let Some(p) = presets::PRESETS.iter().find(|p| env_var(p.id) == var)
                && a.provider.preset().map(|q| q.id) != Some(p.id)
            {
                return err(format!("the id \"{}\" is taken by the provider {} (choose another)", a.id, p.name));
            }
        }
        if let Some(d) = &self.default
            && !self.id_taken(d)
        {
            return err(format!("the default account \"{d}\" doesn't exist"));
        }
        Ok(())
    }

    /// Every account's [`Account::warnings`].
    pub fn warnings(&self) -> Vec<String> {
        self.accounts.iter().flat_map(Account::warnings).collect()
    }

    pub fn get(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// The settings for a run on account `id` with `model` (the thread's;
    /// `None` for the account's default). See [`crate::resolve()`].
    pub fn resolve(&self, id: &str, model: Option<&str>) -> Result<Resolved, ProvidersError> {
        resolve(self.get(id).ok_or_else(|| ProvidersError::UnknownAccount(id.to_string()))?, model)
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
        if !self.id_taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}-{n}")).find(|id| !self.id_taken(id)).expect("some id is free")
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

    fn bad(json: &str) -> String {
        match AccountsFile::from_json(json) {
            Err(ProvidersError::Config(m)) => m,
            other => panic!("expected a config error for {json}, got {other:?}"),
        }
    }

    /// A file with one OpenAI account carrying `extra` (a JSON object's inside).
    fn openai_with(extra: &str) -> String {
        format!(r#"{{"version": 1, "accounts": [{{"id": "a", "provider": "openai", {extra}}}]}}"#)
    }

    #[test]
    fn presets() {
        assert_eq!(Provider::Anthropic.base_url(), "https://api.anthropic.com/v1");
        assert_eq!(Provider::Anthropic.protocol(), Protocol::Anthropic);
        assert_eq!(Provider::Openai.protocol(), Protocol::OpenaiChat);
        assert_eq!(Provider::Openrouter.base_url(), "https://openrouter.ai/api/v1");
        assert_eq!(Provider::Openrouter.name(), "OpenRouter");
        assert_eq!(local().provider.name(), "Custom (OpenAI-compatible)");
        assert!(Provider::Openai.needs_key());
        assert!(!local().provider.needs_key());
        assert_eq!(Provider::Openrouter.default_model(), Some("anthropic/claude-opus-5.5"));
        assert!(!Provider::Openai.models().is_empty());
        assert!(local().provider.models().is_empty());
        assert_eq!(local().provider.keys_url(), None);
    }

    #[test]
    fn ids_round_trip() {
        for p in Provider::PRESETS {
            assert_eq!(Provider::from_id(p.id()), Some(p.clone()));
            let json = serde_json::to_value(&p).unwrap();
            assert_eq!(json["provider"], p.id(), "the file uses the same ids");
        }
        assert_eq!(Provider::from_id(" OpenAI "), Some(Provider::Openai));
        assert_eq!(Provider::from_id("custom"), None);
        assert_eq!(Provider::from_id("gemini"), None);
        assert_eq!(local().provider.id(), "custom");
    }

    #[test]
    fn new_account_takes_the_default_model() {
        assert_eq!(Account::new("a", "A", Provider::Anthropic).model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(local().model, None);
    }

    #[test]
    fn env_var_names() {
        assert_eq!(
            Account::new("work", "Work", Provider::Anthropic).key_env_vars(),
            ["SCRIPTORIUM_WORK_KEY", "SCRIPTORIUM_ANTHROPIC_KEY"]
        );
        assert_eq!(Account::new("anthropic", "A", Provider::Anthropic).key_env_vars(), ["SCRIPTORIUM_ANTHROPIC_KEY"]);
        let mut l = local();
        l.id = "lm-studio.2".into();
        assert_eq!(l.key_env_vars(), ["SCRIPTORIUM_LM_STUDIO_2_KEY"]);
    }

    #[test]
    fn env_var_collisions_are_refused() {
        let mut file = AccountsFile::default();
        file.upsert(Account::new("my-work", "A", Provider::Anthropic)).unwrap();
        let e = file.upsert(Account::new("my_work", "B", Provider::Openai)).unwrap_err();
        assert!(e.to_string().contains("SCRIPTORIUM_MY_WORK_KEY"), "{e}");
        // An id may be its own provider's name, not another's.
        file.upsert(Account::new("anthropic", "A", Provider::Anthropic)).unwrap();
        let e = file.upsert(Account::new("openai", "Proxy", Provider::Anthropic)).unwrap_err();
        assert!(e.to_string().contains("provider OpenAI"), "{e}");
        let mut custom = local();
        custom.id = "OpenRouter".into();
        assert!(file.upsert(custom).is_err());
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
        let v: Value = serde_json::from_str(&json).unwrap();
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
            {"id": "or", "provider": "openrouter", "model": "anthropic/claude-opus-5-5",
             "extra_headers": {"HTTP-Referer": "https://example.org"}}
        ]}"#;
        let file = AccountsFile::from_json(json).unwrap();
        let a = &file.accounts[0];
        assert_eq!(a.provider, Provider::Openrouter);
        assert_eq!(a.name, "or", "the name defaults to the id");
        assert!(a.extra.is_empty(), "{:?}", a.extra);
        assert_eq!(file.default_account().unwrap().id, "or", "the only account is the default");
    }

    #[test]
    fn what_this_version_doesnt_know_survives_a_rewrite() {
        let json = r#"{"version": 1, "default": "g", "sync": {"every": 5}, "accounts": [
            {"id": "g", "name": "Gemini", "provider": "gemini", "region": "eu", "model": "gemini-9"},
            {"id": "a", "name": "A", "provider": "anthropic", "future_field": [1, 2], "model": "claude-opus-5-5"}
        ]}"#;
        let mut file = AccountsFile::from_json(json).unwrap();
        assert_eq!(file.accounts.len(), 1, "the unknown provider's account is hidden");
        assert_eq!(file.accounts[0].extra["future_field"], serde_json::json!([1, 2]));
        assert_eq!(file.default_account(), None, "the default is the hidden account");
        assert_eq!(file.unused_id("g"), "g-2", "hidden ids stay taken");
        assert!(file.upsert(Account::new("g", "G", Provider::Openai)).is_err());

        // Edit what this version knows, write, and nothing else is lost.
        let mut a = file.accounts[0].clone();
        a.model = Some("claude-sonnet-5-5".into());
        file.upsert(a).unwrap();
        let written: Value = serde_json::from_str(&file.to_json()).unwrap();
        let original: Value = serde_json::from_str(json).unwrap();
        assert_eq!(written["sync"], original["sync"]);
        assert_eq!(written["default"], "g");
        let accounts = written["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[1], original["accounts"][0], "the hidden account is written back as it was");
        assert_eq!(accounts[0]["future_field"], serde_json::json!([1, 2]));
        assert_eq!(accounts[0]["model"], "claude-sonnet-5-5");
        assert_eq!(accounts[0].as_object().unwrap().keys().filter(|k| *k == "provider").count(), 1);
        assert_eq!(AccountsFile::from_json(&file.to_json()).unwrap(), file);
    }

    #[test]
    fn refuses_bad_files() {
        assert!(bad(r#"{"version": 2, "accounts": []}"#).contains("version 2"));
        assert!(bad(r#"{"version": 0}"#).contains("version 0"));
        assert!(bad("not json").contains("expected"));
        assert!(bad(r#"{"version": 1, "accounts": [{"id": "x"}]}"#).contains("no provider"));
        bad(r#"{"version": 1, "accounts": [{"id": "c", "provider": "custom", "protocol": "openai_chat"}]}"#);
        bad(r#"{"version": 1, "accounts": [{"provider": "openai"}]}"#);
        let custom = |url: &str| {
            format!(
                r#"{{"version": 1, "accounts": [{{"id": "c", "provider": "custom", "protocol": "anthropic", "base_url": "{url}"}}]}}"#
            )
        };
        assert!(bad(&custom("localhost:8080")).contains("http"));
        assert!(bad(&custom("https://")).contains("http"));
        assert!(bad(&custom("https://exa mple.org/v1")).contains("spaces"));
        assert!(bad(&custom("https://example.org/v1\\n")).contains("spaces"));
        assert!(
            bad(r#"{"version": 1, "accounts": [{"id": "a", "provider": "anthropic"}, {"id": "a", "provider": "openai"}]}"#)
                .contains("two accounts")
        );
        assert!(bad(r#"{"version": 1, "default": "x", "accounts": []}"#).contains("default"));
        bad(r#"{"version": 1, "accounts": [{"id": " ", "provider": "openai"}]}"#);
        bad(&openai_with(r#""max_tokens": 0"#));
        bad(&openai_with(r#""model": " ""#));
        bad(&openai_with(r#""prices": {"m": {"input": -1, "output": 1}}"#));
    }

    #[test]
    fn credentials_cant_hide_in_headers() {
        for header in [
            r#""Authorization": "Bearer sk-1""#,
            r#""authorization ": "Bearer sk-1""#,
            r#"" Authorization": "Bearer sk-1""#,
            r#""PROXY-AUTHORIZATION": "x""#,
            r#""Cookie": "x""#,
            r#""x-goog-api-key": "x""#,
            r#""OpenAI-Api-Key": "x""#,
            r#""X-Api-Key": "x""#,
            r#""Host": "evil.example""#,
            r#""X-Title:Authorization": "x""#,
            r#""": "x""#,
            r#""X-Note": "a\r\nAuthorization: Bearer sk-1""#,
            r#""X-A": "1", "x-a": "2""#,
        ] {
            let m = bad(&openai_with(&format!(r#""extra_headers": {{{header}}}"#)));
            assert!(m.contains("header"), "{header}: {m}");
        }
        AccountsFile::from_json(&openai_with(r#""extra_headers": {"HTTP-Referer": "https://x.org", "X-T": "a\tb"}"#))
            .unwrap();
    }

    #[test]
    fn plain_http_to_another_machine_is_a_warning() {
        let custom = |url: &str| {
            Account::new("c", "C", Provider::Custom { protocol: Protocol::OpenaiChat, base_url: url.into() })
        };
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080",
            "http://[::1]:8080/v1",
            "http://192.168.1.20/v1",
            "http://10.0.0.5",
            "http://172.20.0.2",
            "http://box.local/v1",
            "https://api.example.org/v1",
        ] {
            assert!(custom(url).warnings().is_empty(), "{url}");
        }
        for url in ["http://api.example.org/v1", "HTTP://8.8.8.8", "http://172.32.0.1", "http://localhost.example.org"]
        {
            assert_eq!(custom(url).warnings().len(), 1, "{url}");
        }
        let mut file = AccountsFile::default();
        file.upsert(custom("http://api.example.org/v1")).unwrap();
        assert_eq!(file.warnings().len(), 1);
    }

    #[test]
    fn a_model_without_chat_tools_warns() {
        let mut a = Account::new("o", "OpenAI", Provider::Openai);
        assert!(a.warnings().is_empty(), "the default model calls tools");
        for model in ["gpt-6-astra", "gpt-6.1-sol", "gpt-6.1-terra"] {
            a.model = Some(model.into());
            assert_eq!(a.refuses_tools(), Some(model));
            let w = a.warnings();
            assert!(w.len() == 1 && w[0].contains(model) && w[0].contains("tools"), "{w:?}");
            assert!(!a.insecure_http());
        }
        a.model = Some("gpt-6-sol".into());
        assert!(a.warnings().is_empty(), "sent without reasoning instead");
        let mut or = Account::new("or", "OpenRouter", Provider::Openrouter);
        or.model = Some("openai/gpt-6.1-sol".into());
        assert!(or.warnings().is_empty(), "OpenRouter's ids aren't in the table");
    }

    #[test]
    fn editing_accounts() {
        let mut file = AccountsFile::default();
        file.upsert(Account::new("anthropic", "Anthropic", Provider::Anthropic)).unwrap();
        assert_eq!(file.unused_id("anthropic"), "anthropic-2");
        assert_eq!(file.unused_id("work"), "work");

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
        assert_eq!(file.resolve("anthropic", None).unwrap().model, "claude-sonnet-5-5");
        assert_eq!(file.resolve("nope", None), Err(ProvidersError::UnknownAccount("nope".into())));
        assert_eq!(file.remove("anthropic").unwrap().name, "Work");
        assert_eq!(file.default, None);
        assert_eq!(file.remove("anthropic"), None);
        assert_eq!(file.default_account().unwrap().id, "ollama");
    }
}
