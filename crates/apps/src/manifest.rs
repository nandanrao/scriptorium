//! An app's manifest, `apps/<name>/app.json`, and the rule for app names.
//!
//! ```json
//! { "scriptorium": 1, "title": "Scene list", "description": "Every scene, following the writer." }
//! ```
//!
//! `scriptorium` must be `1` (the format); `title` and `description` are
//! required strings; other keys are ignored. A problem found in a folder
//! (no manifest, no `index.html`, a bad name) is a [`Problem`] too, so the
//! list shows every reason an app can't open in one place.

use std::fmt;

use serde_json::Value;

/// The manifest format this build reads.
pub const FORMAT: u64 = 1;

/// The longest app name (the folder name).
pub const NAME_MAX: usize = 40;

/// What an app's `app.json` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The menu item and the page title.
    pub title: String,
    pub description: String,
}

/// Why an app can't be opened, in words for the writer and the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The folder name isn't `[a-z0-9][a-z0-9-]{0,39}`.
    BadName(String),
    /// The folder has no `app.json`.
    NoManifest,
    /// The folder has no `index.html`, the entry page.
    NoIndex,
    /// `app.json` isn't JSON (the parser's message).
    BadJson(String),
    /// `app.json` is JSON but not an object.
    NotObject,
    /// `scriptorium` is missing.
    NoFormat,
    /// `scriptorium` isn't `1` (what it is, as JSON).
    WrongFormat(String),
    /// A required field is missing.
    Missing(&'static str),
    /// A required field isn't a string.
    NotString(&'static str),
    /// A required field is empty (or only spaces).
    Empty(&'static str),
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::BadName(name) => write!(
                f,
                "the folder name \"{name}\" isn't allowed: use 1 to {NAME_MAX} lowercase letters, digits and \
                 hyphens, starting with a letter or digit"
            ),
            Problem::NoManifest => write!(f, "app.json is missing"),
            Problem::NoIndex => write!(f, "index.html is missing"),
            Problem::BadJson(e) => write!(f, "app.json isn't valid JSON: {e}"),
            Problem::NotObject => write!(f, "app.json must be a JSON object"),
            Problem::NoFormat => write!(f, "app.json needs \"scriptorium\": {FORMAT}"),
            Problem::WrongFormat(v) => write!(f, "app.json has \"scriptorium\": {v}; this Scriptorium reads {FORMAT}"),
            Problem::Missing(k) => write!(f, "app.json needs \"{k}\""),
            Problem::NotString(k) => write!(f, "app.json's \"{k}\" must be a string"),
            Problem::Empty(k) => write!(f, "app.json's \"{k}\" is empty"),
        }
    }
}

impl std::error::Error for Problem {}

/// Reads `app.json`. Every problem found is returned, not just the first,
/// so an agent fixes them in one go.
pub fn parse(text: &str) -> Result<Manifest, Vec<Problem>> {
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Err(vec![Problem::BadJson(e.to_string())]),
    };
    let Some(obj) = value.as_object() else {
        return Err(vec![Problem::NotObject]);
    };
    let mut problems = Vec::new();
    match obj.get("scriptorium") {
        None => problems.push(Problem::NoFormat),
        Some(v) if v.as_u64() == Some(FORMAT) => {}
        Some(v) => problems.push(Problem::WrongFormat(v.to_string())),
    }
    let mut field = |key: &'static str| -> String {
        match obj.get(key) {
            None => problems.push(Problem::Missing(key)),
            Some(Value::String(s)) if s.trim().is_empty() => problems.push(Problem::Empty(key)),
            Some(Value::String(s)) => return s.trim().to_string(),
            Some(_) => problems.push(Problem::NotString(key)),
        }
        String::new()
    };
    let title = field("title");
    let description = field("description");
    if problems.is_empty() { Ok(Manifest { title, description }) } else { Err(problems) }
}

/// Whether `name` may name an app: `[a-z0-9][a-z0-9-]{0,39}`. It is the
/// folder name and a URL part, so it never needs escaping anywhere.
pub fn name_ok(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= NAME_MAX
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_good_manifest() {
        let m = parse(r#"{"scriptorium": 1, "title": " Scene list ", "description": "Every scene.", "x": 3}"#).unwrap();
        assert_eq!(m.title, "Scene list");
        assert_eq!(m.description, "Every scene.");
    }

    #[test]
    fn every_problem_at_once() {
        let p = parse(r#"{"scriptorium": 2, "title": 3}"#).unwrap_err();
        assert_eq!(
            p,
            vec![Problem::WrongFormat("2".into()), Problem::NotString("title"), Problem::Missing("description")]
        );
    }

    #[test]
    fn bad_json_and_not_object() {
        assert!(matches!(parse("{").unwrap_err()[..], [Problem::BadJson(_)]));
        assert_eq!(parse("[1]").unwrap_err(), vec![Problem::NotObject]);
        assert_eq!(
            parse(r#"{"title": "", "description": "d"}"#).unwrap_err(),
            vec![Problem::NoFormat, Problem::Empty("title")]
        );
        assert_eq!(
            parse(r#"{"scriptorium": "1", "title": "t", "description": "d"}"#).unwrap_err(),
            vec![Problem::WrongFormat("\"1\"".into())]
        );
        assert_eq!(
            parse(r#"{"scriptorium": 1.0, "title": "t", "description": "d"}"#).unwrap_err(),
            vec![Problem::WrongFormat("1.0".into())]
        );
    }

    #[test]
    fn names() {
        for ok in ["a", "scene-list", "0x", "a-", &"a".repeat(40)] {
            assert!(name_ok(ok), "{ok}");
        }
        for bad in ["", "-a", "Scene", "a_b", "a.b", "a b", "é", ".git", &"a".repeat(41)] {
            assert!(!name_ok(bad), "{bad}");
        }
    }

    #[test]
    fn problems_read_plainly() {
        assert_eq!(Problem::Missing("title").to_string(), "app.json needs \"title\"");
        assert!(Problem::BadName("X".into()).to_string().contains("\"X\""));
    }
}
