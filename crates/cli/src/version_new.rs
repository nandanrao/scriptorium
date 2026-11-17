//! `scriptorium version new`: make a version lined up with a script (plan
//! step A5b). The work is [`scriptorium_fsio::versions::version_new`], which
//! the tool hosts call too; this parses the arguments, holds the original's
//! session lock, and prints the summary and the version's absolute path.

use std::path::Path;

use scriptorium_fsio::versions::{check_version_args, version_new};

use crate::commands::{USAGE, lock};

/// `version new <script.fdx> --lang <code> [--from <code>] [-o <name.fdx>]`
/// (the arguments after `version new`).
pub(crate) fn command(args: &[&str], notes: &mut Vec<String>) -> Result<String, String> {
    let (mut script, mut lang, mut from, mut name) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        let mut value = |flag: &str| it.next().copied().ok_or(format!("{flag} needs a value"));
        match a {
            "--lang" => lang = Some(value(a)?),
            "--from" => from = Some(value(a)?),
            "-o" => name = Some(value(a)?),
            other if other.starts_with('-') || script.is_some() => {
                return Err(format!("unexpected argument {other:?}\n\n{USAGE}"));
            }
            other => script = Some(other),
        }
    }
    let script = script.ok_or(format!("version new: which script?\n\n{USAGE}"))?;
    let lang = lang.ok_or("version new: give the version's language with --lang <code> (es, fr, pt-BR, ...)")?;
    // Names and codes first: nothing (not even the lock) is written for a
    // bad one.
    let script_name = Path::new(script).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = name.map(|n| out_name(script, n)).transpose()?;
    check_version_args(&script_name, lang, from, name.as_deref())?;
    let _lock = lock(script)?;
    let made = version_new(Path::new(script), lang, from, name.as_deref(), notes)?;
    Ok(format!("{}\n{}\n", made.summary(), made.version.display()))
}

/// `-o` names a file in the script's folder. A plain name is used as is; a
/// path (`./x.fdx`, `/abs/x.fdx`) is accepted when its folder is the
/// script's, else refused.
fn out_name(script: &str, o: &str) -> Result<String, String> {
    if !o.contains(['/', '\\']) {
        return Ok(o.to_string());
    }
    let at = Path::new(o);
    let file = at.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = match at.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.canonicalize().ok(),
        _ => Path::new(".").canonicalize().ok(),
    };
    let dir = scriptorium_fsio::opened::script_dir(script)?;
    if folder.as_deref() != Some(dir.as_path()) || file.is_empty() {
        return Err(format!("-o {o}: a version goes in the script's folder ({}); give a file name", dir.display()));
    }
    Ok(file)
}
