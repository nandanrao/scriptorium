//! API keys: in the OS keyring (feature `keyring`: Secret Service on Linux,
//! the Keychain on macOS, Credential Manager on Windows), else from the
//! account's environment variables ([`Account::key_env_vars`]:
//! `SCRIPTORIUM_<ACCOUNT ID>_KEY`, then `SCRIPTORIUM_<PROVIDER>_KEY`). Never
//! in a plain-text file.
//!
//! A variable that is set wins: it's the explicit choice (an agent, a test,
//! CI, a bare D-Bus session), and when one is set the keyring isn't touched
//! at all. Otherwise the keyring is asked.
//!
//! **Never call [`Keys`] on the UI thread.** Asking the keyring can block:
//! a locked Secret Service collection waits for the writer to unlock it, and
//! with no Secret Service on the bus a lookup can take about 25 s to fail.
//! Keyring answers (including "couldn't ask") are cached for the process, so
//! that happens at most once per account.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::account::Account;

/// The keyring service name entries are stored under; the entry's user is
/// the account id.
pub const SERVICE: &str = "scriptorium";

/// Where a key came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    /// From this environment variable.
    Env(String),
}

/// What the accounts sheet and `scriptorium accounts list` say about a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
    /// "in keychain"
    Keyring,
    /// "from SCRIPTORIUM_ANTHROPIC_KEY"
    Env(String),
    /// "missing". `keyring` says why the keyring couldn't be asked, if it
    /// couldn't (no Secret Service, locked, built without it); `env_vars`
    /// are the variables that would supply the key, in lookup order.
    Missing { env_vars: Vec<String>, keyring: Option<String> },
}

/// Storing or deleting a key failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// There's no keyring to keep keys in; set one of these variables
    /// instead (in lookup order).
    NoKeyring { env_vars: Vec<String>, reason: String },
    /// The keyring refused.
    Keyring(String),
    /// The key is empty.
    Empty,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::NoKeyring { env_vars, reason } => {
                write!(f, "there's no keyring to keep the key in ({reason}); set {} instead", env_vars.join(" or "))
            }
            KeyError::Keyring(m) => write!(f, "the keyring refused: {m}"),
            KeyError::Empty => write!(f, "the key is empty"),
        }
    }
}

impl std::error::Error for KeyError {}

type Env = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

enum Store {
    /// The OS keyring.
    #[cfg(feature = "keyring")]
    System,
    /// No keyring: the reason, for messages.
    None(&'static str),
    /// A keyring in memory, for tests of code that stores keys.
    Memory(Mutex<HashMap<String, String>>),
}

/// Reads and stores API keys.
pub struct Keys {
    store: Store,
    env: Env,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let store = match &self.store {
            #[cfg(feature = "keyring")]
            Store::System => "system",
            Store::None(_) => "none",
            Store::Memory(_) => "memory",
        };
        f.debug_struct("Keys").field("store", &store).finish_non_exhaustive()
    }
}

/// A non-empty value, trimmed.
fn nonempty(v: String) -> Option<String> {
    let t = v.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Set to `off` to keep keys out of the OS keyring: only the
/// `SCRIPTORIUM_<ACCOUNT>_KEY` / `SCRIPTORIUM_<PROVIDER>_KEY` variables are
/// read, and nothing on the machine's keyring is read or changed (CI, a
/// session without a keyring, tests). The CLI and the app both honour it,
/// through [`Keys::from_env`].
pub const KEYRING_ENV: &str = "SCRIPTORIUM_KEYRING";

/// Why there is no keyring with [`KEYRING_ENV`]`=off` ([`Keys::env_only`]),
/// so a program can tell that choice from a keyring that failed.
pub const KEYRING_OFF_REASON: &str = "keyring not used";

/// Whether [`KEYRING_ENV`] is `off` (any case, spaces ignored).
pub fn keyring_off() -> bool {
    std::env::var(KEYRING_ENV).is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

impl Keys {
    /// The keys a program should use: [`Keys::system`], or
    /// [`Keys::env_only`] with `SCRIPTORIUM_KEYRING=off`.
    pub fn from_env() -> Keys {
        if keyring_off() { Keys::env_only() } else { Keys::system() }
    }

    /// The OS keyring (when built with it) and the process environment.
    pub fn system() -> Keys {
        #[cfg(feature = "keyring")]
        let store = Store::System;
        #[cfg(not(feature = "keyring"))]
        let store = Store::None("this build has no keyring support");
        Keys { store, env: Box::new(|k| std::env::var(k).ok()) }
    }

    /// The process environment only: never touches the keyring.
    pub fn env_only() -> Keys {
        Keys { store: Store::None(KEYRING_OFF_REASON), env: Box::new(|k| std::env::var(k).ok()) }
    }

    /// An empty in-memory keyring and the environment `env` describes:
    /// for tests (nothing on the machine is read or changed).
    pub fn in_memory(env: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Keys {
        Keys { store: Store::Memory(Mutex::new(HashMap::new())), env: Box::new(env) }
    }

    /// The account's key and where it came from, if there is one.
    pub fn get(&self, account: &Account) -> Option<(String, KeySource)> {
        self.lookup(account).0
    }

    pub fn status(&self, account: &Account) -> KeyStatus {
        match self.lookup(account) {
            (Some((_, KeySource::Keyring)), _) => KeyStatus::Keyring,
            (Some((_, KeySource::Env(v))), _) => KeyStatus::Env(v),
            (None, keyring) => KeyStatus::Missing { env_vars: account.key_env_vars(), keyring },
        }
    }

    /// The key, or why the keyring couldn't be asked.
    fn lookup(&self, account: &Account) -> (Option<(String, KeySource)>, Option<String>) {
        for var in account.key_env_vars() {
            if let Some(k) = (self.env)(&var).and_then(nonempty) {
                return (Some((k, KeySource::Env(var))), None);
            }
        }
        match self.read(&account.id) {
            Ok(k) => (k.and_then(nonempty).map(|k| (k, KeySource::Keyring)), None),
            Err(e) => (None, Some(e)),
        }
    }

    /// Ok(None): the keyring has no entry. Err: it couldn't be asked.
    fn read(&self, id: &str) -> Result<Option<String>, String> {
        match &self.store {
            #[cfg(feature = "keyring")]
            Store::System => {
                system::cached(id, || match keyring::Entry::new(SERVICE, id).and_then(|e| e.get_password()) {
                    Ok(k) => Ok(Some(k)),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(e) => Err(e.to_string()),
                })
            }
            Store::None(why) => Err((*why).to_string()),
            Store::Memory(m) => Ok(m.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()),
        }
    }

    /// Keeps `key` for the account in the keyring (surrounding whitespace
    /// dropped).
    pub fn set(&self, account: &Account, key: &str) -> Result<(), KeyError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(KeyError::Empty);
        }
        match &self.store {
            #[cfg(feature = "keyring")]
            Store::System => {
                keyring::Entry::new(SERVICE, &account.id)
                    .and_then(|e| e.set_password(key))
                    .map_err(|e| system_error(account, e))?;
                system::remember(&account.id, Ok(Some(key.to_string())));
                Ok(())
            }
            Store::None(why) => Err(KeyError::NoKeyring { env_vars: account.key_env_vars(), reason: (*why).into() }),
            Store::Memory(m) => {
                m.lock().unwrap_or_else(|e| e.into_inner()).insert(account.id.clone(), key.to_string());
                Ok(())
            }
        }
    }

    /// Removes the account's key from the keyring; no key is fine. (A key
    /// in an environment variable is the environment's business.)
    pub fn delete(&self, account: &Account) -> Result<(), KeyError> {
        match &self.store {
            #[cfg(feature = "keyring")]
            Store::System => match keyring::Entry::new(SERVICE, &account.id).and_then(|e| e.delete_credential()) {
                Ok(()) | Err(keyring::Error::NoEntry) => {
                    system::remember(&account.id, Ok(None));
                    Ok(())
                }
                Err(e) => Err(system_error(account, e)),
            },
            Store::None(_) => Ok(()),
            Store::Memory(m) => {
                m.lock().unwrap_or_else(|e| e.into_inner()).remove(&account.id);
                Ok(())
            }
        }
    }
}

/// The process-wide cache of keyring answers, by account id.
#[cfg(feature = "keyring")]
mod system {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    type Answer = Result<Option<String>, String>;

    fn cache() -> &'static Mutex<HashMap<String, Answer>> {
        static CACHE: OnceLock<Mutex<HashMap<String, Answer>>> = OnceLock::new();
        CACHE.get_or_init(Default::default)
    }

    /// The cached answer for `id`, or `ask` (outside the lock) and cache it.
    pub fn cached(id: &str, ask: impl FnOnce() -> Answer) -> Answer {
        if let Some(a) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(id) {
            return a.clone();
        }
        let a = ask();
        remember(id, a.clone());
        a
    }

    pub fn remember(id: &str, a: Answer) {
        cache().lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), a);
    }
}

#[cfg(feature = "keyring")]
fn system_error(account: &Account, e: keyring::Error) -> KeyError {
    match e {
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => {
            KeyError::NoKeyring { env_vars: account.key_env_vars(), reason: e.to_string() }
        }
        e => KeyError::Keyring(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Provider;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + Send + Sync {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn a_set_variable_wins_over_the_keyring() {
        let work = Account::new("work", "Work", Provider::Anthropic);
        let home = Account::new("home", "Home", Provider::Openai);
        let keys =
            Keys::in_memory(env(&[("SCRIPTORIUM_ANTHROPIC_KEY", " sk-env \n"), ("SCRIPTORIUM_OPENAI_KEY", " ")]));
        keys.set(&work, "  sk-stored ").unwrap();
        assert_eq!(keys.get(&work), Some(("sk-env".into(), KeySource::Env("SCRIPTORIUM_ANTHROPIC_KEY".into()))));
        assert_eq!(keys.status(&work), KeyStatus::Env("SCRIPTORIUM_ANTHROPIC_KEY".into()));
        // A blank variable counts as unset.
        keys.set(&home, "sk-stored").unwrap();
        assert_eq!(keys.get(&home), Some(("sk-stored".into(), KeySource::Keyring)));
        assert_eq!(keys.status(&home), KeyStatus::Keyring);
        keys.delete(&home).unwrap();
        keys.delete(&home).unwrap();
        assert_eq!(keys.get(&home), None);
        assert_eq!(keys.set(&work, "  "), Err(KeyError::Empty));
    }

    #[test]
    fn the_accounts_own_variable_comes_first() {
        let work = Account::new("work", "Work", Provider::Anthropic);
        let keys = Keys::in_memory(env(&[("SCRIPTORIUM_ANTHROPIC_KEY", "shared"), ("SCRIPTORIUM_WORK_KEY", "own")]));
        assert_eq!(keys.get(&work), Some(("own".into(), KeySource::Env("SCRIPTORIUM_WORK_KEY".into()))));
    }

    #[test]
    fn missing_keys_name_the_variable() {
        let a = Account::new("x", "X", Provider::Openrouter);
        let keys = Keys::in_memory(env(&[("SCRIPTORIUM_OPENROUTER_KEY", "")]));
        assert_eq!(keys.get(&a), None);
        assert_eq!(
            keys.status(&a),
            KeyStatus::Missing {
                env_vars: vec!["SCRIPTORIUM_X_KEY".into(), "SCRIPTORIUM_OPENROUTER_KEY".into()],
                keyring: None
            }
        );
    }

    #[test]
    fn without_a_keyring() {
        let a = Account::new("work", "Work", Provider::Anthropic);
        let keys = Keys { store: Store::None("no keyring here"), env: Box::new(env(&[])) };
        assert_eq!(
            keys.status(&a),
            KeyStatus::Missing {
                env_vars: vec!["SCRIPTORIUM_WORK_KEY".into(), "SCRIPTORIUM_ANTHROPIC_KEY".into()],
                keyring: Some("no keyring here".into())
            }
        );
        let e = keys.set(&a, "sk").unwrap_err();
        assert!(matches!(&e, KeyError::NoKeyring { env_vars, .. } if env_vars.len() == 2));
        assert!(e.to_string().contains("SCRIPTORIUM_WORK_KEY or SCRIPTORIUM_ANTHROPIC_KEY"), "{e}");
        keys.delete(&a).unwrap();
    }
}
