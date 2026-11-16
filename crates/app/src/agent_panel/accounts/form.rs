//! The accounts sheet's form as plain values: what the writer typed in, and
//! the [`Account`] it makes (or the sentence saying what to fix). No UI, no
//! keyring, no files, so it is tested alone.

use scriptorium_agent::{Effort, Protocol};
use scriptorium_providers::shell::{KEYRING_ENV, KEYRING_OFF_REASON, KeyStatus, config};
use scriptorium_providers::{Account, AccountsFile, Provider};

/// Which provider an account is for (the pick when adding one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Anthropic,
    Openai,
    Openrouter,
    Custom,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Anthropic, Kind::Openai, Kind::Openrouter, Kind::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Anthropic => "Anthropic",
            Kind::Openai => "OpenAI",
            Kind::Openrouter => "OpenRouter",
            Kind::Custom => "Custom",
        }
    }

    pub fn of(provider: &Provider) -> Kind {
        match provider {
            Provider::Anthropic => Kind::Anthropic,
            Provider::Openai => Kind::Openai,
            Provider::Openrouter => Kind::Openrouter,
            Provider::Custom { .. } => Kind::Custom,
        }
    }

    fn named(self) -> Option<Provider> {
        match self {
            Kind::Anthropic => Some(Provider::Anthropic),
            Kind::Openai => Some(Provider::Openai),
            Kind::Openrouter => Some(Provider::Openrouter),
            Kind::Custom => None,
        }
    }

    pub fn id(self) -> &'static str {
        self.named().map_or("custom", |p| p.id())
    }

    /// Suggested models for the preset picker (empty for a custom endpoint).
    pub fn models(self) -> &'static [&'static str] {
        self.named().map_or(&[], |p| p.models())
    }

    pub fn keys_url(self) -> Option<&'static str> {
        self.named().and_then(|p| p.keys_url())
    }
}

/// The reasoning-effort choices, in the order to list them.
pub const EFFORTS: [Option<Effort>; 6] =
    [None, Some(Effort::Low), Some(Effort::Medium), Some(Effort::High), Some(Effort::XHigh), Some(Effort::Max)];

pub fn effort_label(e: Option<Effort>) -> &'static str {
    e.map_or("Model default", Effort::name)
}

pub const PROTOCOLS: [Protocol; 2] = [Protocol::OpenaiChat, Protocol::Anthropic];

/// What the writer has entered for one account.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    /// The id of the account being edited; `None` for a new one.
    pub editing: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    pub effort: Option<Effort>,
    /// As typed; empty means the default.
    pub max_tokens: String,
}

impl Form {
    pub fn new(kind: Kind) -> Form {
        Form {
            editing: None,
            kind,
            name: String::new(),
            protocol: Protocol::OpenaiChat,
            base_url: String::new(),
            model: kind.named().and_then(|p| p.default_model()).unwrap_or("").to_string(),
            effort: None,
            max_tokens: String::new(),
        }
    }

    pub fn from_account(a: &Account) -> Form {
        Form {
            editing: Some(a.id.clone()),
            kind: Kind::of(&a.provider),
            name: if a.name.trim().is_empty() { a.id.clone() } else { a.name.clone() },
            protocol: a.provider.protocol(),
            base_url: if let Provider::Custom { base_url, .. } = &a.provider {
                base_url.clone()
            } else {
                String::new()
            },
            model: a.model.clone().unwrap_or_default(),
            effort: a.effort,
            max_tokens: a.max_tokens.map(|n| n.to_string()).unwrap_or_default(),
        }
    }

    fn provider(&self) -> Provider {
        self.kind
            .named()
            .unwrap_or_else(|| Provider::Custom { protocol: self.protocol, base_url: self.base_url.trim().to_string() })
    }

    /// The id a new account gets: the name as a slug, else the provider's.
    /// Never another provider's id (those are reserved for its own accounts).
    fn id_base(&self) -> String {
        let slug = slug(&self.name);
        if slug.is_empty() {
            self.kind.id().to_string()
        } else if Provider::from_id(&slug).is_some_and(|p| Kind::of(&p) != self.kind) {
            format!("custom-{slug}")
        } else {
            slug
        }
    }

    /// For a new account: what its id is built from. The write picks the
    /// id that is unused in the file as it is then (see `work::save_account`);
    /// an id chosen earlier may have been taken by then.
    pub fn create_base(&self) -> Option<String> {
        self.editing.is_none().then(|| self.id_base())
    }

    fn new_id(&self, file: &AccountsFile) -> String {
        file.unused_id(&self.id_base())
    }

    /// The account as entered, without checking it. `base` is the account
    /// being edited, whose other settings (headers, prices) carry over.
    fn draft(&self, id: String, base: Option<&Account>) -> Account {
        let provider = self.provider();
        let name = self.name.trim();
        let mut a = base.cloned().unwrap_or_else(|| Account::new(&id, "", provider.clone()));
        a.id = id;
        a.name = if name.is_empty() { provider.name() } else { name.to_string() };
        a.provider = provider;
        let model = self.model.trim();
        a.model = (!model.is_empty()).then(|| model.to_string());
        a.effort = self.effort;
        a.max_tokens = self.max_tokens.trim().parse().ok().filter(|n| *n > 0);
        a
    }

    fn id_for(&self, file: &AccountsFile) -> String {
        self.editing.clone().unwrap_or_else(|| self.new_id(file))
    }

    /// Something worth telling the writer now, before saving: plain http to
    /// another machine, or a model that can't call tools.
    pub fn warnings(&self, file: &AccountsFile, base: Option<&Account>) -> Vec<String> {
        plain_warnings(&self.draft(self.id_for(file), base))
    }

    /// The environment variables the key could come from (for "Advanced").
    pub fn env_vars(&self, file: &AccountsFile, base: Option<&Account>) -> Vec<String> {
        self.draft(self.id_for(file), base).key_env_vars()
    }

    /// The account to save, or what to fix first.
    pub fn account(&self, file: &AccountsFile, base: Option<&Account>) -> Result<Account, String> {
        if self.kind == Kind::Custom {
            let url = self.base_url.trim();
            if url.is_empty() {
                return Err("Enter the base URL, for example http://localhost:11434/v1.".into());
            }
            let lower = url.to_ascii_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                return Err("The base URL must start with http:// or https://.".into());
            }
            if self.model.trim().is_empty() {
                return Err("Enter a model for this endpoint.".into());
            }
        }
        let max = self.max_tokens.trim();
        if !max.is_empty() && !max.parse::<u64>().is_ok_and(|n| n > 0) {
            return Err("The output limit must be a whole number above 0, or empty for the default.".into());
        }
        let mut a = self.draft(self.id_for(file), base);
        if a.model.is_none() {
            a.model = a.provider.default_model().map(String::from);
        }
        // The file's own check: ids, env-var names, URLs, headers.
        file.clone().upsert(a.clone()).map_err(|e| e.to_string())?;
        Ok(a)
    }
}

/// Warnings about an account in plain words (the library's own carry the
/// account's internal id).
pub fn plain_warnings(a: &Account) -> Vec<String> {
    let mut out = vec![];
    if a.insecure_http() {
        out.push("This address isn't secure (http). Your key and script text would be sent unencrypted.".into());
    }
    if let Ok(r) = scriptorium_providers::resolve(a, None) {
        out.extend(r.warnings().into_iter().map(|w| format!("{}.", capitalised(&w))));
    }
    out
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// A friendlier name for a suggested model id.
pub fn model_label(id: &str) -> String {
    scriptorium_providers::model_name(id).map_or_else(|| id.to_string(), String::from)
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// How a key looks to the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Plain,
    Bad,
}

/// The short status for the list: "in keychain", "from your environment",
/// "missing" (or "no key" for an endpoint that may not need one).
pub fn key_short(status: &KeyStatus, needs_key: bool) -> (String, Tone) {
    match status {
        KeyStatus::Keyring => ("in keychain".into(), Tone::Good),
        KeyStatus::Env(_) => ("from your environment".into(), Tone::Good),
        KeyStatus::Missing { .. } if needs_key => ("missing".into(), Tone::Bad),
        KeyStatus::Missing { .. } => ("no key".into(), Tone::Plain),
    }
}

/// The help under the key field, in plain words (no variable names: see
/// [`key_advanced`]). `provider` is the provider's name.
pub fn key_long(status: &KeyStatus, needs_key: bool, provider: &str) -> (String, Tone) {
    match status {
        KeyStatus::Keyring => ("A key is saved in your keychain. Leave this empty to keep it.".into(), Tone::Good),
        KeyStatus::Env(_) => {
            ("Using a key from your environment, which takes priority over one saved here.".into(), Tone::Good)
        }
        KeyStatus::Missing { keyring: Some(why), .. } if why == KEYRING_OFF_REASON => (
            "The keychain is turned off for this app, so a key can't be saved here. See Advanced for another way."
                .into(),
            Tone::Bad,
        ),
        KeyStatus::Missing { keyring: Some(_), .. } => (
            "Your system's keychain isn't available, so a key can't be saved here. See Advanced for another way."
                .into(),
            Tone::Bad,
        ),
        KeyStatus::Missing { .. } if needs_key => (format!("Paste your key from {provider}."), Tone::Plain),
        KeyStatus::Missing { .. } => ("This endpoint may not need a key. Paste one if it does.".into(), Tone::Plain),
    }
}

/// The technical detail for the "Advanced" disclosure.
pub fn key_advanced(env_vars: &[String], status: Option<&KeyStatus>) -> String {
    let mut text = format!(
        "Instead of the keychain, the key can come from the environment variable {}. A variable that is set is used first.",
        env_vars.join(" or ")
    );
    match status {
        Some(KeyStatus::Missing { keyring: Some(why), .. }) if why == KEYRING_OFF_REASON => {
            text.push_str(&format!(" The keychain is turned off ({KEYRING_ENV}=off)."));
        }
        Some(KeyStatus::Missing { keyring: Some(why), .. }) => {
            text.push_str(&format!(" Why the keychain can't be used: {why}."));
        }
        _ => {}
    }
    text
}

/// The accounts file's path from the environment, or why there isn't one.
pub fn accounts_path() -> Result<std::path::PathBuf, String> {
    config::default_path().ok_or_else(|| "There's no home folder to keep the accounts file in.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_with(accounts: &[Account]) -> AccountsFile {
        let mut f = AccountsFile::default();
        for a in accounts {
            f.upsert(a.clone()).unwrap();
        }
        f
    }

    #[test]
    fn new_anthropic_account_takes_preset_defaults() {
        let f = Form::new(Kind::Anthropic);
        assert_eq!(f.model, "claude-opus-5-5");
        let a = f.account(&AccountsFile::default(), None).unwrap();
        assert_eq!((a.id.as_str(), a.name.as_str()), ("anthropic", "Anthropic"));
        assert_eq!(a.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(a.effort, None);
    }

    #[test]
    fn ids_come_from_the_name_and_stay_unique() {
        let mut f = Form::new(Kind::Anthropic);
        f.name = "  Work (Team) ".into();
        let file = AccountsFile::default();
        assert_eq!(f.account(&file, None).unwrap().id, "work-team");
        let taken = file_with(&[Account::new("work-team", "x", Provider::Anthropic)]);
        assert_eq!(f.account(&taken, None).unwrap().id, "work-team-2");
        // A second account with no name: the provider's id, then numbered.
        let g = Form::new(Kind::Anthropic);
        let one = file_with(&[Account::new("anthropic", "A", Provider::Anthropic)]);
        assert_eq!(g.account(&one, None).unwrap().id, "anthropic-2");
        // A name that is another provider's id can't be used as an id.
        let mut c = Form::new(Kind::Custom);
        c.name = "OpenAI".into();
        c.base_url = "http://localhost:1/v1".into();
        c.model = "m".into();
        assert_eq!(c.account(&file, None).unwrap().id, "custom-openai");
    }

    #[test]
    fn editing_keeps_the_id_and_what_the_form_doesnt_show() {
        let mut a = Account::new("work", "Work", Provider::Anthropic);
        a.extra_headers.insert("X-Team".into(), "film".into());
        a.max_tokens = Some(8000);
        let file = file_with(&[a.clone()]);
        let mut f = Form::from_account(&a);
        assert_eq!(f.max_tokens, "8000");
        f.name = "Renamed".into();
        f.model = "claude-sonnet-5-5".into();
        f.effort = Some(Effort::High);
        f.max_tokens = "".into();
        let b = f.account(&file, Some(&a)).unwrap();
        assert_eq!((b.id.as_str(), b.name.as_str()), ("work", "Renamed"));
        assert_eq!(b.extra_headers["X-Team"], "film");
        assert_eq!((b.effort, b.max_tokens), (Some(Effort::High), None));
    }

    #[test]
    fn custom_needs_url_and_model() {
        let file = AccountsFile::default();
        let mut f = Form::new(Kind::Custom);
        assert!(f.account(&file, None).unwrap_err().contains("base URL"));
        f.base_url = "localhost:8080".into();
        assert!(f.account(&file, None).unwrap_err().contains("http://"));
        f.base_url = " http://localhost:11434/v1 ".into();
        assert!(f.account(&file, None).unwrap_err().contains("model"));
        f.model = "qwen3".into();
        let a = f.account(&file, None).unwrap();
        assert_eq!(a.id, "custom");
        assert_eq!(a.provider.base_url(), "http://localhost:11434/v1", "trimmed");
        assert_eq!(a.provider.protocol(), Protocol::OpenaiChat);
        f.protocol = Protocol::Anthropic;
        assert_eq!(f.account(&file, None).unwrap().provider.protocol(), Protocol::Anthropic);
    }

    #[test]
    fn output_limit_must_be_a_positive_number() {
        let file = AccountsFile::default();
        let mut f = Form::new(Kind::Openai);
        for bad in ["abc", "0", "-5", "1.5"] {
            f.max_tokens = bad.into();
            assert!(f.account(&file, None).unwrap_err().contains("output limit"), "{bad}");
        }
        f.max_tokens = " 4096 ".into();
        assert_eq!(f.account(&file, None).unwrap().max_tokens, Some(4096));
    }

    #[test]
    fn empty_model_falls_back_for_a_named_provider() {
        let mut f = Form::new(Kind::Openai);
        f.model = "  ".into();
        assert_eq!(f.account(&AccountsFile::default(), None).unwrap().model.as_deref(), Some("gpt-6-sol"));
        f.model = "gpt-9-custom".into();
        assert_eq!(f.account(&AccountsFile::default(), None).unwrap().model.as_deref(), Some("gpt-9-custom"));
    }

    #[test]
    fn warns_about_plain_http_to_another_machine_while_typing() {
        let file = AccountsFile::default();
        let mut f = Form::new(Kind::Custom);
        f.base_url = "http://api.example.org/v1".into();
        assert_eq!(f.warnings(&file, None).len(), 1, "even before a model is entered");
        f.base_url = "http://localhost:11434/v1".into();
        assert!(f.warnings(&file, None).is_empty());
        f.base_url = "https://api.example.org/v1".into();
        assert!(f.warnings(&file, None).is_empty());
        assert!(Form::new(Kind::Anthropic).warnings(&file, None).is_empty());
    }

    #[test]
    fn the_files_own_check_still_applies() {
        let mut f = Form::new(Kind::Custom);
        f.base_url = "https://exa mple.org".into();
        f.model = "m".into();
        assert!(f.account(&AccountsFile::default(), None).unwrap_err().contains("spaces"));
    }

    #[test]
    fn key_status_wording() {
        let missing = KeyStatus::Missing {
            env_vars: vec!["SCRIPTORIUM_WORK_KEY".into(), "SCRIPTORIUM_ANTHROPIC_KEY".into()],
            keyring: None,
        };
        assert_eq!(key_short(&KeyStatus::Keyring, true).0, "in keychain");
        assert_eq!(key_short(&KeyStatus::Env("SCRIPTORIUM_X_KEY".into()), true).0, "from your environment");
        assert_eq!(key_short(&missing, true), ("missing".into(), Tone::Bad));
        assert_eq!(key_short(&missing, false), ("no key".into(), Tone::Plain));
        let (long, tone) = key_long(&missing, true, "Anthropic");
        assert_eq!((long.as_str(), tone), ("Paste your key from Anthropic.", Tone::Plain));
        assert!(!key_long(&KeyStatus::Env("SCRIPTORIUM_X_KEY".into()), true, "A").0.contains("SCRIPTORIUM"));
        let no_ring = KeyStatus::Missing { env_vars: vec!["SCRIPTORIUM_A_KEY".into()], keyring: Some("no bus".into()) };
        assert!(!key_long(&no_ring, true, "A").0.contains("no bus"));
        let adv = key_advanced(&["SCRIPTORIUM_WORK_KEY".into(), "SCRIPTORIUM_ANTHROPIC_KEY".into()], Some(&no_ring));
        assert!(adv.contains("SCRIPTORIUM_WORK_KEY or SCRIPTORIUM_ANTHROPIC_KEY") && adv.contains("no bus"), "{adv}");
        // Turned off on purpose: said so, not as a failure.
        let off = KeyStatus::Missing { env_vars: vec![], keyring: Some(KEYRING_OFF_REASON.into()) };
        assert!(key_long(&off, true, "A").0.contains("turned off"));
        let adv = key_advanced(&["SCRIPTORIUM_A_KEY".into()], Some(&off));
        assert!(adv.contains("turned off") && !adv.contains(KEYRING_OFF_REASON), "{adv}");
    }

    #[test]
    fn warnings_use_plain_words_without_ids() {
        let file = AccountsFile::default();
        let mut f = Form::new(Kind::Custom);
        f.name = "Proxy box".into();
        f.base_url = "http://api.example.org/v1".into();
        let w = f.warnings(&file, None);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("isn't secure") && !w[0].contains("proxy-box"), "{w:?}");
    }

    #[test]
    fn new_accounts_build_their_id_from_a_base() {
        let mut f = Form::new(Kind::Anthropic);
        assert_eq!(f.create_base().as_deref(), Some("anthropic"));
        f.editing = Some("anthropic".into());
        assert_eq!(f.create_base(), None);
        assert_eq!(model_label("claude-opus-5-5"), "Claude Opus 5.5");
        assert_eq!(model_label("something-else"), "something-else");
    }
}
