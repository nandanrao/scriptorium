//! `scriptorium apps <script> [--json]`: the apps in `<script folder>/apps/`
//! (see `scriptorium_apps::list`), with exit 1 when any has problems.

use std::path::Path;

use scriptorium_apps::{list, shell};

use crate::commands::{EXIT_ERROR, EXIT_USAGE, Failed, USAGE};

/// The commands this file handles, dispatched from `commands::run_command`.
pub const COMMANDS: &[&str] = &["apps"];

pub fn command(args: &[&str]) -> Result<String, Failed> {
    let (script, json) = match args {
        [script] => (*script, false),
        [script, "--json"] | ["--json", script] => (*script, true),
        _ => return Err(Failed::new(EXIT_USAGE, format!("wrong arguments for `apps`\n\n{USAGE}"))),
    };
    let path = Path::new(script);
    if !path.is_file() {
        return Err(Failed::from(format!("{script}: no such script")));
    }
    let full = std::path::absolute(path).map_err(|e| Failed::from(format!("{script}: {e}")))?;
    let folder = full.parent().unwrap_or(Path::new("/"));
    let apps_dir = folder.join("apps");
    let found = shell::scan(folder);
    let mut out = if json {
        serde_json::to_string(&list::json(&found, None)).expect("serializable")
    } else {
        list::text(&apps_dir, &found, None)
    };
    out.push('\n');
    if found.iter().all(|a| a.ok()) {
        return Ok(out);
    }
    let bad: Vec<&str> = found.iter().filter(|a| !a.ok()).map(|a| a.name.as_str()).collect();
    Err(Failed { message: format!("apps with problems: {}", bad.join(", ")), stdout: out, code: EXIT_ERROR })
}
