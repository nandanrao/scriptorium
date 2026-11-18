//! The project key, and request paths turned into checked parts.
//!
//! A path is percent-decoded first, then split on `/`, then each part is
//! checked, so an encoded `%2e%2e` or `%2F` is caught like a plain one.
//! These checks are on the request alone; the shell still refuses links
//! when it opens the file (A3).

use std::fmt;
use std::path::Path;

/// One checked part of a path: not empty, not `.` or `..`, not starting
/// with `.`, and free of `/`, `\`, `:` and NUL.
pub type Part = String;

/// A path that was refused, and why (for logs and tests; the answer is 404).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Doesn't start with `/`.
    NotAbsolute,
    /// A `%` not followed by two hex digits.
    BadEscape,
    /// The decoded bytes aren't UTF-8.
    NotUtf8,
    /// An empty part (`a//b`, or a path ending `//`).
    EmptyPart,
    /// `.` or `..`.
    DotPart,
    /// A part starting with `.` (`.git`, `.env`).
    Hidden,
    /// A part holding `\`, `:` or NUL.
    BadChar,
    /// A static path into the app's `data/`.
    DataAsStatic,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refused::NotAbsolute => "the path doesn't start with /",
            Refused::BadEscape => "a % isn't followed by two hex digits",
            Refused::NotUtf8 => "the path isn't UTF-8",
            Refused::EmptyPart => "the path has an empty part",
            Refused::DotPart => "the path has a . or .. part",
            Refused::Hidden => "the path has a part starting with .",
            Refused::BadChar => "the path has a \\, : or NUL",
            Refused::DataAsStatic => "an app's data/ isn't served as a page",
        })
    }
}

impl std::error::Error for Refused {}

/// The key naming a project folder in URLs: 16 lowercase hex digits of
/// FNV-1a 64 over the folder path's bytes. The caller passes the canonical
/// folder, so the key is stable across restarts. For correctness (a page
/// never acts on another folder's script), not security.
pub fn project_key(folder: &Path) -> String {
    format!("{:016x}", fnv1a(folder.as_os_str().as_encoded_bytes()))
}

/// Whether `key` looks like a [`project_key`].
pub fn key_ok(key: &str) -> bool {
    key.len() == 16 && key.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// FNV-1a 64: small, stable, no dependency. Also used for file stamps.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Percent-decodes `raw`, a URL path without its query, then splits it.
///
/// `raw` must start with `/`; one trailing `/` is allowed and dropped (so
/// `/p/k/n/` gives `["p", "k", "n"]`; use [`ends_in_slash`] to tell it from
/// `/p/k/n`). `/` alone gives no parts.
pub fn decode_and_split(raw: &str) -> Result<Vec<Part>, Refused> {
    let decoded = percent_decode(raw)?;
    let Some(rest) = decoded.strip_prefix('/') else {
        return Err(Refused::NotAbsolute);
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    rest.split('/').map(check_part).collect()
}

/// Whether the decoded path ends in `/` (a folder, as `/p/<key>/<name>/`).
pub fn ends_in_slash(raw: &str) -> bool {
    raw.ends_with('/') || raw.to_ascii_lowercase().ends_with("%2f")
}

fn check_part(part: &str) -> Result<Part, Refused> {
    if part.is_empty() {
        Err(Refused::EmptyPart)
    } else if part == "." || part == ".." {
        Err(Refused::DotPart)
    } else if part.starts_with('.') {
        Err(Refused::Hidden)
    } else if part.contains(['\\', ':', '\0']) {
        Err(Refused::BadChar)
    } else {
        Ok(part.to_string())
    }
}

/// Decodes `%XX` escapes. `+` stays `+` (it means a space only in forms).
pub fn percent_decode(raw: &str) -> Result<String, Refused> {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = |c: u8| (c as char).to_digit(16);
            match (b.get(i + 1).and_then(|c| hex(*c)), b.get(i + 2).and_then(|c| hex(*c))) {
                (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                _ => return Err(Refused::BadEscape),
            }
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| Refused::NotUtf8)
}

/// A file inside `apps/<name>/` from the parts after the app's name: no
/// parts means `index.html`. `data/…` is refused (any case: a
/// case-insensitive disk would serve `Data/` from `data/`).
pub fn static_path(parts: &[Part]) -> Result<Vec<Part>, Refused> {
    match parts.first() {
        None => Ok(vec!["index.html".to_string()]),
        Some(first) if first.eq_ignore_ascii_case("data") => Err(Refused::DataAsStatic),
        Some(_) => Ok(parts.to_vec()),
    }
}

/// A file inside `apps/<name>/data/` from the parts after `data`. No parts
/// means the folder itself (the list).
pub fn data_path(parts: &[Part]) -> Vec<Part> {
    parts.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(raw: &str) -> Result<Vec<String>, Refused> {
        decode_and_split(raw)
    }

    #[test]
    fn plain_paths() {
        assert_eq!(split("/").unwrap(), Vec::<String>::new());
        assert_eq!(split("/p/k/n/").unwrap(), ["p", "k", "n"]);
        assert_eq!(split("/p/k/n/img/a%20b.png").unwrap(), ["p", "k", "n", "img", "a b.png"]);
        assert_eq!(split("/a+b").unwrap(), ["a+b"]);
        assert_eq!(split("/caf%C3%A9").unwrap(), ["café"]);
    }

    #[test]
    fn refused_paths() {
        assert_eq!(split("/a/../b"), Err(Refused::DotPart));
        assert_eq!(split("/../etc/passwd"), Err(Refused::DotPart));
        assert_eq!(split("/a/%2e%2e/b"), Err(Refused::DotPart));
        assert_eq!(split("/a/%2E%2E%2Fb"), Err(Refused::DotPart));
        assert_eq!(split("/a/./b"), Err(Refused::DotPart));
        assert_eq!(split("/a/.git/config"), Err(Refused::Hidden));
        assert_eq!(split("/a/%2egit"), Err(Refused::Hidden));
        assert_eq!(split("/a//etc/passwd"), Err(Refused::EmptyPart));
        assert_eq!(split("//etc"), Err(Refused::EmptyPart));
        assert_eq!(split("/a/b\\..\\c"), Err(Refused::BadChar));
        assert_eq!(split("/a/%5c"), Err(Refused::BadChar));
        assert_eq!(split("/a/C:"), Err(Refused::BadChar));
        assert_eq!(split("/a/%00"), Err(Refused::BadChar));
        assert_eq!(split("a/b"), Err(Refused::NotAbsolute));
        assert_eq!(split("%2Fa"), Ok(vec!["a".to_string()]));
        assert_eq!(split("/a/%2"), Err(Refused::BadEscape));
        assert_eq!(split("/a/%zz"), Err(Refused::BadEscape));
        assert_eq!(split("/a/%ff"), Err(Refused::NotUtf8));
    }

    #[test]
    fn an_encoded_slash_splits_like_a_plain_one() {
        // Decoded first, so `%2F` can't hide a `..` inside one part.
        assert_eq!(split("/a%2Fb").unwrap(), ["a", "b"]);
        assert_eq!(split("/a/..%2Fsecret"), Err(Refused::DotPart));
        assert!(ends_in_slash("/p/k/n%2F"));
        assert!(!ends_in_slash("/p/k/n"));
    }

    #[test]
    fn static_and_data() {
        let parts = |s: &[&str]| s.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert_eq!(static_path(&[]).unwrap(), ["index.html"]);
        assert_eq!(static_path(&parts(&["app.js"])).unwrap(), ["app.js"]);
        assert_eq!(static_path(&parts(&["data", "x.json"])), Err(Refused::DataAsStatic));
        assert_eq!(static_path(&parts(&["Data", "x.json"])), Err(Refused::DataAsStatic));
        assert_eq!(static_path(&parts(&["data"])), Err(Refused::DataAsStatic));
        assert_eq!(static_path(&parts(&["vendor", "data"])).unwrap(), ["vendor", "data"]);
        assert_eq!(data_path(&parts(&["img", "a.png"])), ["img", "a.png"]);
    }

    #[test]
    fn keys() {
        let k = project_key(Path::new("/home/w/film"));
        assert!(key_ok(&k), "{k}");
        assert_eq!(k, project_key(Path::new("/home/w/film")));
        assert_ne!(k, project_key(Path::new("/home/w/film2")));
        // FNV-1a 64 of "" and "a" (published test vectors).
        assert_eq!(fnv1a(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a(b"a"), 0xaf63dc4c8601ec8c);
        assert!(!key_ok("ABCDEF0123456789"));
        assert!(!key_ok("abc"));
    }
}
