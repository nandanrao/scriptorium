//! `scriptorium`: the headless command line. Everything the app can do is
//! reachable from here, so coding agents can use Scriptorium as fully as people
//! do, and anyone can script it and check their work without the GUI.

mod review_cmds;

use std::collections::BTreeMap;
use std::process::ExitCode;

use scriptorium_core::outline::annotate;
use scriptorium_fdx::FdxFile;
use serde::Serialize;

const USAGE: &str = "\
scriptorium: screenplay tools

SCRIPTS
  scriptorium info <script.fdx>          summary: paragraphs, scenes, characters
  scriptorium dump <script.fdx> [--json] every paragraph with its id, type, scene and speaker
  scriptorium roundtrip <script.fdx>     check that a no-op save is byte-identical

REVIEWING
  These work on the same saved session as the app (<script>.scriptorium.json
  next to the script), so an agent and a person see each other's decisions.
  The session remembers which review files are attached to the script; a
  command uses those when you don't name one. Review files named on a command
  that saves (decide, edit, undo, redo, apply) are attached too.
  Suggestions are numbered from 1 in script order, as `suggestions` lists them.
  <review> below is a review file, or --review <name or file> to pick one of
  the attached reviews; it can be left out when only one is attached.

  scriptorium reviews <script.fdx>
        the attached review files, with progress
  scriptorium reviews <script.fdx> add <review.txt>...
  scriptorium reviews <script.fdx> remove <review.txt>...
        attach or detach review files (detaching keeps the text as it is)
  scriptorium review <script.fdx> [<review.txt>...]
        summary of each review: suggestions, decisions, lines not found
  scriptorium suggestions <script.fdx> [<review>] [--pending] [--json]
        list the suggestions, with status and current text
  scriptorium decide <script.fdx> [<review>] <n> accept [<option>]
  scriptorium decide <script.fdx> [<review>] <n> keep | undecide
  scriptorium decide <script.fdx> [<review>] <n> rewrite <text>
        decide suggestion n (option numbers start at 1)
  scriptorium edit <script.fdx> <paragraph id> <text> [-r <review.txt>]...
        replace one paragraph's text (ids from `dump`)
  scriptorium undo <script.fdx> [-r <review.txt>]...
  scriptorium redo <script.fdx> [-r <review.txt>]...
  scriptorium suggest <review.txt> <script.fdx> (--para <id> | --before <text>)
                      --after <text> [--after <text>]... [--why <text>]
        add a suggestion to a review file (created if missing); checks that
        the line exists and that the change would be visible
  scriptorium progress <script.fdx> [<review>] [-o <file>]
        the review file with every decision written in (STATUS/CHOSEN/MINE),
        to hand on or continue elsewhere; stdout unless -o
  scriptorium apply <script.fdx> [<review.txt>...] [--accept-all] [-o <out.fdx>]
        write the script with the session's decisions and edits
        (--accept-all: also take option 1 of every pending suggestion).
        Default output: \"<script> (scriptorium).fdx\" next to the script
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["info", path] => load(path).map(|f| info(&f)),
        ["dump", path] => load(path).map(|f| dump(&f, false)),
        ["dump", path, "--json"] => load(path).map(|f| dump(&f, true)),
        ["roundtrip", path] => load(path).and_then(|f| roundtrip(&f)),
        [cmd, rest @ ..] if review_cmds::COMMANDS.contains(cmd) => review_cmds::run(cmd, rest),
        ["-h" | "--help" | "help"] => {
            print!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("unrecognized arguments\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn load(path: &str) -> Result<FdxFile, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    FdxFile::parse(raw).map_err(|e| format!("{path}: {e}"))
}

fn info(f: &FdxFile) {
    let script = f.to_script();
    let ctx = annotate(&script);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut speakers: BTreeMap<String, usize> = BTreeMap::new();
    for (p, c) in script.paragraphs.iter().zip(&ctx) {
        *kinds.entry(p.kind.name().to_string()).or_default() += 1;
        if !c.speaker.is_empty() && p.kind.name() == "Dialogue" {
            *speakers.entry(c.speaker.clone()).or_default() += 1;
        }
    }
    let words: usize = script.paragraphs.iter().map(|p| p.text().split_whitespace().count()).sum();
    println!("paragraphs  {}", script.paragraphs.len());
    println!("scenes      {}", ctx.iter().filter(|c| c.heading).count());
    println!("words       {words}");
    println!("\nelements");
    for (k, n) in &kinds {
        println!("  {n:>5}  {k}");
    }
    let mut top: Vec<_> = speakers.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!("\ncharacters (dialogue paragraphs)");
    for (name, n) in top.iter().take(15) {
        println!("  {n:>5}  {name}");
    }
}

#[derive(Serialize)]
struct Row<'a> {
    id: u64,
    kind: &'a str,
    scene: &'a str,
    speaker: &'a str,
    text: String,
}

fn dump(f: &FdxFile, json: bool) {
    let script = f.to_script();
    let ctx = annotate(&script);
    let rows: Vec<Row> = script
        .paragraphs
        .iter()
        .zip(&ctx)
        .map(|(p, c)| Row { id: p.id.0, kind: p.kind.name(), scene: &c.scene, speaker: &c.speaker, text: p.text() })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows).expect("rows serialize"));
    } else {
        for r in rows {
            println!("{:>5}  {:<14} {}", r.id, r.kind, r.text);
        }
    }
}

fn roundtrip(f: &FdxFile) -> Result<(), String> {
    let out = f.write_script(&f.to_script()).map_err(|e| e.to_string())?;
    if out == f.raw() {
        println!("ok: no-op save is byte-identical ({} bytes, {} paragraphs)", out.len(), f.paragraphs().len());
        Ok(())
    } else {
        Err("no-op save changed the file".into())
    }
}
