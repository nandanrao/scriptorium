//! What the accounts sheet does off the UI thread: read and write the
//! accounts file, ask the keyring, send the "Check" request. Each function
//! blocks (a locked Linux keyring can wait about 25 s), so the sheet only
//! calls them from a background task. They re-read the file each time, so
//! an edit made with `scriptorium accounts` meanwhile is not lost.

use std::collections::HashMap;
use std::path::Path;

use scriptorium_agent::protocol;
use scriptorium_providers::shell::{CancelToken, Ended, Flow, KeyStatus, Keys, config, transport_from_env};
use scriptorium_providers::{Account, AccountsFile, models_request};

/// The file and each account's key status, as of one moment.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub file: AccountsFile,
    pub statuses: HashMap<String, KeyStatus>,
}

/// A change that was made, and what went wrong around it, if anything
/// (the account is saved even when its key couldn't be stored).
#[derive(Debug, Clone)]
pub struct Done {
    pub snapshot: Snapshot,
    pub problem: Option<String>,
}

/// The keys the sheet uses. `SCRIPTORIUM_TEST_KEYS=memory` (kept in this
/// process only) or `=env` replace the keyring for tests that must not
/// touch the real one.
pub fn keys_from_env() -> Keys {
    match std::env::var("SCRIPTORIUM_TEST_KEYS").as_deref() {
        Ok("memory") => Keys::in_memory(|k| std::env::var(k).ok()),
        Ok("env") => Keys::env_only(),
        _ => Keys::system(),
    }
}

pub fn snapshot(path: &Path, keys: &Keys) -> Result<Snapshot, String> {
    let file = config::load(path).map_err(|e| e.to_string())?;
    let statuses = file.accounts.iter().map(|a| (a.id.clone(), keys.status(a))).collect();
    Ok(Snapshot { file, statuses })
}

/// Adds or replaces `account`, then stores `key` if one was typed. The
/// first account becomes the default; so does one asked to be.
pub fn save_account(
    path: &Path,
    keys: &Keys,
    account: Account,
    key: Option<&str>,
    make_default: bool,
) -> Result<Done, String> {
    let mut file = config::load(path).map_err(|e| e.to_string())?;
    let first = file.accounts.is_empty();
    file.upsert(account.clone()).map_err(|e| e.to_string())?;
    if first || make_default {
        file.set_default(&account.id).map_err(|e| e.to_string())?;
    }
    config::save(path, &file).map_err(|e| e.to_string())?;
    let problem = key
        .filter(|k| !k.trim().is_empty())
        .and_then(|k| keys.set(&account, k).err())
        .map(|e| format!("The account is saved, but its key was not: {e}."));
    Ok(Done { snapshot: snapshot(path, keys)?, problem })
}

/// Removes an account and its keychain entry.
pub fn remove_account(path: &Path, keys: &Keys, id: &str) -> Result<Done, String> {
    let mut file = config::load(path).map_err(|e| e.to_string())?;
    let Some(account) = file.remove(id) else {
        return Ok(Done { snapshot: snapshot(path, keys)?, problem: None });
    };
    config::save(path, &file).map_err(|e| e.to_string())?;
    let problem = keys.delete(&account).err().map(|e| format!("The account is removed, but its key was not: {e}."));
    Ok(Done { snapshot: snapshot(path, keys)?, problem })
}

pub fn make_default(path: &Path, keys: &Keys, id: &str) -> Result<Snapshot, String> {
    let mut file = config::load(path).map_err(|e| e.to_string())?;
    file.set_default(id).map_err(|e| e.to_string())?;
    config::save(path, &file).map_err(|e| e.to_string())?;
    snapshot(path, keys)
}

/// Deletes the keychain entry; returns the status afterwards.
pub fn delete_key(keys: &Keys, account: &Account) -> Result<KeyStatus, String> {
    keys.delete(account).map_err(|e| e.to_string())?;
    Ok(keys.status(account))
}

/// "Check": the cheap models request (no tokens, no script text) with the
/// typed key, or else the stored one. Says how it went, in a sentence.
pub fn check(keys: &Keys, account: &Account, typed: Option<&str>) -> Result<String, String> {
    let key =
        typed.map(str::trim).filter(|k| !k.is_empty()).map(String::from).or_else(|| keys.get(account).map(|k| k.0));
    let request = models_request(account, key.as_deref()).map_err(|e| e.to_string())?;
    let transport = transport_from_env(None).map_err(|e| e.to_string())?;
    let mut body = String::new();
    let ended = transport
        .send(&request, &CancelToken::new(), &mut |line| {
            body.push_str(line);
            body.push('\n');
            Flow::Continue
        })
        .map_err(|e| e.to_string())?;
    if ended == Ended::Cancelled {
        return Err("The check was cancelled.".into());
    }
    let models = protocol::parse_models(&body).map_err(|e| e.to_string())?;
    Ok(match models.len() {
        0 => "The address and key work (the endpoint lists no models).".to_string(),
        1 => "The address and key work: 1 model available.".to_string(),
        n => format!("The address and key work: {n} models available."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_providers::Provider;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("scriptorium-app-accounts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("accounts.json")
    }

    fn keys() -> Keys {
        Keys::in_memory(|_| None)
    }

    #[test]
    fn first_account_is_the_default_and_its_key_is_stored() {
        let path = scratch("first");
        let keys = keys();
        let a = Account::new("work", "Work", Provider::Anthropic);
        let done = save_account(&path, &keys, a.clone(), Some(" sk-1 "), false).unwrap();
        assert!(done.problem.is_none());
        assert_eq!(done.snapshot.file.default.as_deref(), Some("work"));
        assert_eq!(done.snapshot.statuses["work"], KeyStatus::Keyring);
        assert_eq!(keys.get(&a).unwrap().0, "sk-1");
        // The file never holds a key.
        assert!(!std::fs::read_to_string(&path).unwrap().contains("sk-1"));

        let b = Account::new("home", "Home", Provider::Openai);
        let done = save_account(&path, &keys, b, None, false).unwrap();
        assert_eq!(done.snapshot.file.default.as_deref(), Some("work"), "a second one isn't");
        assert!(matches!(done.snapshot.statuses["home"], KeyStatus::Missing { .. }));
        let snap = make_default(&path, &keys, "home").unwrap();
        assert_eq!(snap.file.default.as_deref(), Some("home"));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn editing_without_a_key_keeps_the_stored_one() {
        let path = scratch("edit");
        let keys = keys();
        let mut a = Account::new("work", "Work", Provider::Anthropic);
        save_account(&path, &keys, a.clone(), Some("sk-1"), false).unwrap();
        a.model = Some("claude-sonnet-5-5".into());
        let done = save_account(&path, &keys, a.clone(), Some(""), false).unwrap();
        assert_eq!(done.snapshot.file.accounts[0].model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(keys.get(&a).unwrap().0, "sk-1");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_key_that_cant_be_stored_doesnt_lose_the_account() {
        let path = scratch("nokeyring");
        let keys = Keys::env_only();
        let a = Account::new("work", "Work", Provider::Anthropic);
        let done = save_account(&path, &keys, a, Some("sk-1"), false).unwrap();
        assert_eq!(done.snapshot.file.accounts.len(), 1);
        let p = done.problem.unwrap();
        assert!(p.contains("SCRIPTORIUM_WORK_KEY"), "{p}");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn removing_deletes_the_key_and_clears_the_default() {
        let path = scratch("remove");
        let keys = keys();
        let a = Account::new("work", "Work", Provider::Anthropic);
        save_account(&path, &keys, a.clone(), Some("sk-1"), false).unwrap();
        let done = remove_account(&path, &keys, "work").unwrap();
        assert!(done.snapshot.file.accounts.is_empty());
        assert_eq!(done.snapshot.file.default, None);
        assert_eq!(keys.get(&a), None);
        // Removing what is already gone is not an error.
        assert!(remove_account(&path, &keys, "work").unwrap().problem.is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_corrupt_file_is_reported_not_overwritten() {
        let path = scratch("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json").unwrap();
        let a = Account::new("work", "Work", Provider::Anthropic);
        assert!(snapshot(&path, &keys()).is_err());
        assert!(save_account(&path, &keys(), a, None, false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn check_needs_a_key_before_it_sends_anything() {
        let a = Account::new("work", "Work", Provider::Anthropic);
        let e = check(&keys(), &a, Some("  ")).unwrap_err();
        assert!(e.contains("no API key"), "{e}");
    }
}
