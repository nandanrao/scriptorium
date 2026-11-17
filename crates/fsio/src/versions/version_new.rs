//! `version new` (plan step A5b): make a placeholder version of a script
//! beside it, lined up row by row, and the links file between them.
//!
//! Crash safety comes from the order of writes and the re-run rules: the
//! version's `.fdx` is written first (`create_new`, never over a file), then
//! the links file. A re-run looks at what is there:
//!
//! - links file and `.fdx`: the `.fdx` hashing to `b hash:` is "already
//!   made"; otherwise it is re-found (Final Draft saved it, or a re-run after
//!   a write elsewhere), as is the original if it changed;
//! - only the links file: the `.fdx` was renamed, moved or deleted; refused
//!   with the fix (edit the links file to the new name), never deleted: its
//!   rows and stamps are the pair's history;
//! - only the `.fdx` (a crash between the two writes): adopted when its bytes
//!   are exactly what would be written, refused otherwise.
//!
//! Every host (the CLI's `version new`, and later the `new_version` tool in
//! the CLI, MCP and chat hosts) calls [`version_new`], holding the original's
//! [`crate::SessionLock`]; this takes the links lock itself.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use scriptorium_core::ElementKind;
use scriptorium_fdx::FdxFile;
use scriptorium_review::sessionfile;
use scriptorium_versions::links::{self, Links, Member};
use scriptorium_versions::{blank_copy, refind};

use super::pair::{missing, thousands};
use super::{names, refind_message};

use crate::opened::Opened;
use crate::{LOCK_WAIT, LockError, SessionLock, write_atomic_no_link};

/// What [`version_new`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Made {
    /// Both files written now.
    New,
    /// Both were there and the version's bytes are the ones the links file
    /// describes: nothing written.
    Already,
    /// Both were there, but a file's bytes weren't the ones the links file
    /// describes: re-found and the links file rewritten. One message per
    /// file re-found.
    Refound(Vec<String>),
    /// Only the version's `.fdx` was there (a run stopped before writing the
    /// links file), byte for byte what would be written: kept, links file
    /// written.
    Adopted,
}

/// The version [`version_new`] made or found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVersion {
    /// The version's `.fdx`, absolute.
    pub version: PathBuf,
    /// The links file, absolute.
    pub links: PathBuf,
    pub made: Made,
    /// Rows, and of those, lines left blank to translate (scene headings
    /// included), as the links file has them now.
    pub rows: usize,
    pub to_translate: usize,
    /// The version's language name (`Español`), from the links file.
    pub language: String,
    /// The original's language code (`a language:`), if the pair names one.
    pub original_language: Option<String>,
}

impl NewVersion {
    /// What every host tells the user (the CLI prints the version's absolute
    /// path after it).
    pub fn summary(&self) -> String {
        let version = crate::opened::file_name(&self.version);
        let links = crate::opened::file_name(&self.links);
        let counts = format!("{} rows, {} lines to translate", thousands(self.rows), thousands(self.to_translate));
        let mut out = match &self.made {
            Made::New => format!("made {version} ({}) and {links}: {counts}", self.language),
            Made::Already => format!("{version} was already made, with {links}: {counts}"),
            Made::Refound(msgs) => format!("{version} was already made, with {links}. {} {counts}", msgs.join(" ")),
            Made::Adopted => format!(
                "{version} was left by an earlier run that stopped before writing {links}; kept it and wrote {links}: \
                 {counts}"
            ),
        };
        if self.original_language.is_none() {
            out.push_str(&format!(
                "\ntip: --from en names the original's language; add `a language: en` to {links} to name it now"
            ));
        }
        out.push_str(&format!("\nNext: scriptorium rows {:?} --needs, or open it in Scriptorium", version));
        out
    }
}

/// The version file's default name: `<stem>.<lang>.fdx`.
pub fn default_version_name(script_name: &str, lang: &str) -> String {
    let stem = script_name.rsplit_once('.').map_or(script_name, |(stem, _)| stem);
    format!("{stem}.{lang}.fdx")
}

/// The links file of a version: `<version stem>.links.txt`.
pub fn links_name(version_name: &str) -> String {
    let stem = version_name.rsplit_once('.').map_or(version_name, |(stem, _)| stem);
    format!("{stem}.links.txt")
}

/// The links file's lock, `<links stem>.lock` (`PURPLE LIST.es.links.lock`
/// for `PURPLE LIST.es.links.txt`): held from reading the links file to
/// writing it, after any script lock (plan, "Locking"). On a file system
/// without locks: `None`, with a note.
pub fn lock_links(links: &Path, notes: &mut Vec<String>) -> Result<Option<SessionLock>, String> {
    let name = crate::opened::file_name(links);
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
    match SessionLock::acquire(&links.with_file_name(format!("{stem}.lock")), LOCK_WAIT) {
        Ok(l) => Ok(Some(l)),
        Err(e @ LockError::NoLocks) => {
            notes.push(e.to_string());
            Ok(None)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Check `version new`'s arguments before anything is written (a host
/// calls it before taking the script's lock): the codes, the version's name
/// (default [`default_version_name`]) and the script's own file name, which
/// all go into file names and the links file. Returns the version's name.
pub fn check_version_args(
    script_name: &str,
    lang: &str,
    from: Option<&str>,
    name: Option<&str>,
) -> Result<String, String> {
    check_language(lang, "--lang")?;
    if let Some(from) = from {
        check_language(from, "--from")?;
    }
    if script_name.chars().any(char::is_control) {
        return Err(format!("{script_name:?}: a script's name can't hold control characters or line breaks"));
    }
    let version_name = match name {
        Some(n) => n.to_string(),
        None => default_version_name(script_name, lang),
    };
    check_version_name(&version_name, script_name)?;
    Ok(version_name)
}

/// Make a version of `script` in language `lang` beside it (or find the one
/// an earlier run made), following the re-run rules above. `from` is the
/// script's own language; `name` the version's file name in the script's
/// folder (a plain `.fdx` name; default [`default_version_name`]).
///
/// The original is opened from its file and never written; it is refused
/// while its session has changes its file doesn't (plan C8: writing its live
/// document must give back its file byte for byte), and when it is already
/// lined up with another file. A re-run whose `lang` or `from` contradicts
/// the pair made before is refused. The caller holds the original's session
/// lock ([`SessionLock::for_script`]). Things to tell the user go to `notes`.
pub fn version_new(
    script: &Path,
    lang: &str,
    from: Option<&str>,
    name: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<NewVersion, String> {
    let given_name = crate::opened::file_name(script);
    check_version_args(&given_name, lang, from, name)?;
    let script = crate::check_script(script)?;
    let script_str = script.to_string_lossy().into_owned();
    let dir = crate::opened::script_dir(&script_str)?;
    let script_name = crate::opened::file_name(&script);
    let version_name = check_version_args(&script_name, lang, from, name)?;
    let version_path = dir.join(&version_name);
    let links_path = dir.join(links_name(&version_name));
    for at in [&version_path, &links_path] {
        if std::fs::symlink_metadata(at).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a link; move it away or choose another name", at.display()));
        }
    }

    // One pair per script: a links file naming either file that can't be
    // read refuses (it can't say what it lines up)...
    broken_links(&dir, &[&script_name, &version_name])?;

    // The original as its file reads (C8), refused with unsaved changes.
    let o = Opened::open(&script_str, &[], notes)?;
    let as_file = o.fdx.write_script(o.doc.script()).map_err(|e| format!("{script_name}: {e}"))?;
    if as_file != o.raw {
        return Err(format!(
            "{script_name} has changes in Scriptorium that aren't in the file. Write .fdx and save the written file \
             over {script_name}, or undo them."
        ));
    }
    let is_caps = |k: &ElementKind| o.fdx.is_caps(k.name());

    // ...and so does another links file lining either up.
    let version_pair = crate::versions::find_pair(&dir, &version_name).pair;
    for (file, pair) in [(&script_name, o.pair()), (&version_name, version_pair.as_ref())] {
        if let Some(pair) = pair
            && !crate::same_file(&pair.links, &links_path)
        {
            return Err(format!(
                "{file} is already lined up in {} (one version per script for now)",
                crate::opened::file_name(&pair.links)
            ));
        }
    }

    let _links_lock = lock_links(&links_path, notes)?;
    let links_there = links_path.exists();
    let version_there = version_path.exists();

    if links_there {
        let text = std::fs::read_to_string(&links_path).map_err(|e| format!("{}: {e}", links_path.display()))?;
        let pair = parse_ours(&text, &links_path, &script_name, &version_name)?;
        same_languages(&pair, lang, from)?;
        if !version_there {
            // Renamed, moved or deleted: the links file holds the pair's rows
            // and stamps, so it is never deleted here.
            return Err(format!(
                "{}. Or move {} away to make the version again",
                missing(&dir, &text, &crate::opened::file_name(&links_path), 1, &pair),
                crate::opened::file_name(&links_path)
            ));
        }
        let (pair, msgs) = refind_members(pair, &dir, &o.fdx, &is_caps)?;
        let made = if msgs.is_empty() {
            Made::Already
        } else {
            write_links(&links_path, &pair)?;
            Made::Refound(msgs)
        };
        return Ok(result(version_path, links_path, made, &pair));
    }

    let (bytes, pair) = make(&o.fdx, &script_name, &version_name, lang, from, &links_path, &is_caps)?;
    let made = if version_there {
        let there = std::fs::read(&version_path).map_err(|e| format!("{}: {e}", version_path.display()))?;
        if there != bytes.as_bytes() {
            return Err(format!(
                "{} is already there and isn't the version this would make; move it away, or choose another name \
                 with -o",
                version_path.display()
            ));
        }
        Made::Adopted
    } else {
        write_new(&version_path, &bytes)?;
        Made::New
    };
    write_links(&links_path, &pair)?;
    Ok(result(version_path, links_path, made, &pair))
}

fn result(version: PathBuf, links: PathBuf, made: Made, pair: &Links) -> NewVersion {
    let language = pair.members[1].label();
    NewVersion {
        version,
        links,
        made,
        rows: pair.rows.len(),
        to_translate: pair.placeholders(1),
        language,
        original_language: pair.members[0].language.clone(),
    }
}

/// A re-run must ask for the pair that is there: `lang` is the version's
/// language, and `from`, when given, the original's (if the pair names one).
fn same_languages(pair: &Links, lang: &str, from: Option<&str>) -> Result<(), String> {
    let is = |had: &Option<String>, want: &str| had.as_deref().is_some_and(|h| h.eq_ignore_ascii_case(want));
    let b = &pair.members[1];
    if !is(&b.language, lang) {
        return Err(format!(
            "{} is already the version in {}, not in {lang}; give the new one another name with -o",
            b.file,
            b.language.as_deref().unwrap_or("no language")
        ));
    }
    let a = &pair.members[0];
    if let (Some(from), Some(had)) = (from, &a.language)
        && !had.eq_ignore_ascii_case(from)
    {
        return Err(format!("the pair already says {} is in {had}, not {from}", a.file));
    }
    Ok(())
}

/// Refuse when a `*.links.txt` in `dir` names one of `files` (on an `a:`/`b:`
/// line) but doesn't parse: nothing can tell what it lines up, and nothing
/// rewrites it.
fn broken_links(dir: &Path, files: &[&str]) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Ok(()) };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| crate::opened::file_name(p).to_lowercase().ends_with(".links.txt") && p.is_file())
        .collect();
    paths.sort();
    for at in paths {
        let Ok(text) = std::fs::read_to_string(&at) else { continue };
        if let Err(e) = links::parse(&text)
            && let Some(file) = files.iter().find(|f| names(&text, f))
        {
            return Err(format!(
                "{} names {file} but can't be read ({e}); fix it or move it away",
                crate::opened::file_name(&at)
            ));
        }
    }
    Ok(())
}

/// The version's bytes and the links file for them.
fn make(
    original: &FdxFile,
    script_name: &str,
    version_name: &str,
    lang: &str,
    from: Option<&str>,
    links_path: &Path,
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Result<(String, Links), String> {
    let salt = format!(
        "{}\n{}\n{}",
        links_path.display(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()),
        std::process::id()
    );
    let id = format!("lk-{}", &sessionfile::hash(&salt)[..10]);
    let a = Member {
        file: script_name.to_string(),
        language: from.map(str::to_string),
        hash: sessionfile::hash(original.raw()),
        created: None,
    };
    let b =
        Member { file: version_name.to_string(), language: Some(lang.to_string()), hash: String::new(), created: None };
    let (script, mut pair) = blank_copy(&original.to_script(), &id, a, b, is_caps).map_err(|e| e.to_string())?;
    let written = original.write_script(&script).map_err(|e| format!("making {version_name}: {e}"))?;
    // Before hashing: the hash is of the bytes on disk.
    let bytes = scriptorium_fdx::clear_text_offsets(&written);
    pair.members[1].hash = sessionfile::hash(&bytes);
    links::validate(&pair).map_err(|e| format!("making the links file: {e}"))?;
    Ok((bytes, pair))
}

/// The links file at `at`, which must parse and line up exactly
/// `script_name` (`a`) with `version_name` (`b`, made here).
fn parse_ours(text: &str, at: &Path, script_name: &str, version_name: &str) -> Result<Links, String> {
    let name = crate::opened::file_name(at);
    let pair = links::parse(text).map_err(|e| format!("{name} can't be read ({e}); fix it or move it away"))?;
    let ours =
        pair.member_of(script_name) == Some(0) && pair.member_of(version_name) == Some(1) && pair.made() == Some(1);
    if !ours {
        return Err(format!(
            "{name} is already there and doesn't line up {script_name} with a version {version_name} made by \
             `version new`; move it away or choose another name with -o"
        ));
    }
    Ok(pair)
}

/// Re-find each member whose file isn't the bytes `pair` describes. The
/// original's file is `original` (read once, under its session lock); the
/// version is read here, and read again just before the caller writes, so a
/// save in between starts it over (a few times, then gives up).
fn refind_members(
    mut pair: Links,
    dir: &Path,
    original: &FdxFile,
    is_caps: &dyn Fn(&ElementKind) -> bool,
) -> Result<(Links, Vec<String>), String> {
    let mut msgs = vec![];
    let hash = sessionfile::hash(original.raw());
    if pair.members[0].hash != hash {
        let (refound, found) = refind(&pair, 0, &original.to_script(), &hash, is_caps);
        msgs.push(refind_message(&pair.members[0].file, &found));
        pair = refound;
    }
    let at = dir.join(&pair.members[1].file);
    for _ in 0..5 {
        let raw = std::fs::read_to_string(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        let hash = sessionfile::hash(&raw);
        if pair.members[1].hash == hash {
            return Ok((pair, msgs));
        }
        let fdx = FdxFile::parse(raw).map_err(|e| format!("{}: {e}", at.display()))?;
        let (refound, found) = refind(&pair, 1, &fdx.to_script(), &hash, &|k: &ElementKind| fdx.is_caps(k.name()));
        let again = std::fs::read_to_string(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        if sessionfile::hash(&again) == hash {
            msgs.push(refind_message(&pair.members[1].file, &found));
            return Ok((refound, msgs));
        }
    }
    Err(format!("{} keeps changing; try again when it's saved", at.display()))
}

fn write_links(at: &Path, pair: &Links) -> Result<(), String> {
    let text = links::write(pair).map_err(|e| format!("{}: {e}", at.display()))?;
    write_atomic_no_link(at, text.as_bytes()).map_err(|e| format!("{}: {e}", at.display()))
}

/// Write a file that must not exist yet (never over one: `create_new`). If
/// the write fails, the partial file this made is removed.
fn write_new(at: &Path, bytes: &str) -> Result<(), String> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(at).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} appeared meanwhile; run it again", at.display()),
        _ => format!("{}: {e}", at.display()),
    })?;
    if let Err(e) = f.write_all(bytes.as_bytes()).and_then(|()| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(at);
        return Err(format!("{}: {e}", at.display()));
    }
    if let Some(d) = at.parent().and_then(|d| std::fs::File::open(d).ok()) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// A language code, as it goes into a file name and the links file: letters,
/// digits and `-`/`_`, up to 35 (`es`, `es-MX`, `pt_BR`).
fn check_language(code: &str, flag: &str) -> Result<(), String> {
    let ok = (1..=35).contains(&code.len())
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && code.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric());
    if ok { Ok(()) } else { Err(format!("{flag}: {code:?} isn't a language code (like es, fr or pt-BR)")) }
}

/// The version's name: a plain `.fdx` name in the script's folder, not the
/// script's own.
fn check_version_name(name: &str, script_name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || name.contains(['/', '\\', ':'])
        || name.starts_with('.')
        || name.chars().any(char::is_control)
    {
        return Err(format!("{name:?}: give the version a file name in the script's folder, not a path"));
    }
    if !name.to_lowercase().ends_with(".fdx") {
        return Err(format!("{name}: a version is a Final Draft file; give it a name ending in .fdx"));
    }
    if name.to_lowercase() == script_name.to_lowercase() {
        return Err(format!("{name} is the script itself; give the version another name"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(default_version_name("PURPLE LIST.fdx", "es"), "PURPLE LIST.es.fdx");
        assert_eq!(links_name("PURPLE LIST.es.fdx"), "PURPLE LIST.es.links.txt");
    }

    #[test]
    fn languages_and_names_checked() {
        assert!(check_language("pt-BR", "--lang").is_ok());
        assert!(check_language("", "--lang").is_err());
        assert!(check_language("e s", "--lang").is_err());
        assert!(check_language("../x", "--lang").is_err());
        assert!(check_language("-x", "--lang").is_err());
        assert!(check_version_name("a/b.fdx", "film.fdx").is_err());
        assert!(check_version_name(".x.fdx", "film.fdx").is_err());
        assert!(check_version_name("x.txt", "film.fdx").is_err());
        assert!(check_version_name("FILM.fdx", "film.fdx").is_err());
        assert!(check_version_name("film.es.fdx", "film.fdx").is_ok());
        assert!(check_version_name("a\nb: x.fdx", "film.fdx").is_err());
        assert!(check_version_name("a\tb.fdx", "film.fdx").is_err());
        assert!(check_version_args("fi\nlm.fdx", "es", None, None).is_err());
        assert!(check_language("es\n", "--lang").is_err());
    }
}
