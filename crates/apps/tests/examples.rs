//! The example apps (`examples/scene-list`, `examples/characters-by-scene`),
//! which guide('app') points agents at: they follow the guide's own rules.

use std::fs;
use std::path::Path;

use scriptorium_apps::{LIBRARY, manifest};

const EXAMPLES: [&str; 2] = ["scene-list", "characters-by-scene"];

fn read(app: &str, file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join(app).join(file);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every file of an example that the browser loads.
fn sources(app: &str) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join(app);
    let mut out: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".html") || n.ends_with(".js") || n.ends_with(".css"))
        .map(|n| (n.clone(), read(app, &n)))
        .collect();
    out.sort();
    out
}

#[test]
fn each_example_has_a_good_manifest_and_an_index() {
    for app in EXAMPLES {
        assert!(manifest::name_ok(app), "{app}");
        let m = manifest::parse(&read(app, "app.json")).unwrap_or_else(|p| panic!("{app}: {p:?}"));
        assert!(!m.title.is_empty() && !m.description.is_empty(), "{app}");
        assert!(!read(app, "index.html").is_empty(), "{app}");
    }
}

/// The rules guide('app') gives, checked on one page's source `text`
/// (`what` names it in failures).
fn keeps_the_rules(what: &str, text: &str) {
    // Script text only through textContent.
    for bad in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write"] {
        assert!(!text.contains(bad), "{what}: {bad}");
    }
    // No code from text.
    for bad in ["eval(", "new Function", "WebAssembly"] {
        assert!(!text.contains(bad), "{what}: {bad}");
    }
    // Nothing from the internet (the SVG namespace is a name, not a load).
    let text = text.replace("http://www.w3.org/2000/svg", "");
    for bad in ["http://", "https://", "//cdn", "fonts.googleapis"] {
        assert!(!text.contains(bad), "{what}: {bad}");
    }
    // Calls go through the library, never a posted form or a hand-made request.
    assert!(!text.to_lowercase().contains("method=\"post\""), "{what}");
    assert!(!text.to_lowercase().contains("method=post"), "{what}");
    assert!(!text.contains("fetch("), "{what}");
}

/// The library is the first thing in `<head>`.
fn loads_the_library_first(what: &str, html: &str) {
    let head = html.split("<head>").nth(1).unwrap_or_else(|| panic!("{what}: no <head>")).trim_start();
    assert!(head.starts_with(r#"<script src="/scriptorium.js"></script>"#), "{what}");
}

/// Each `S.<name>` in `js` (where `const S = Scriptorium;`) is something
/// the library exports, and the page follows the writer and the script.
fn uses_only_the_library(what: &str, js: &str) {
    let exported = LIBRARY.split("window.Scriptorium = {").nth(1).unwrap().split('}').next().unwrap();
    let exported: Vec<&str> = exported.split(',').map(str::trim).collect();
    assert!(js.contains("const S = Scriptorium"), "{what}");
    for (i, _) in js.match_indices("S.") {
        if js[..i].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let word: String = js[i + 2..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        assert!(exported.contains(&word.as_str()), "{what}: S.{word}");
    }
    assert!(js.contains(r#"S.on("writer""#) && js.contains(r#"S.on("script""#), "{what}");
    assert!(js.contains("S.paragraphs(") && js.contains(".textContent"), "{what}");
}

#[test]
fn each_example_loads_the_library_first() {
    for app in EXAMPLES {
        loads_the_library_first(app, &read(app, "index.html"));
    }
}

#[test]
fn each_example_keeps_the_guides_rules() {
    for app in EXAMPLES {
        for (file, text) in sources(app) {
            keeps_the_rules(&format!("{app}/{file}"), &text);
        }
    }
}

#[test]
fn the_examples_use_only_what_the_library_has() {
    for app in EXAMPLES {
        uses_only_the_library(app, &read(app, "app.js"));
    }
}

/// guide('app')'s text, as the tools crate serves it.
const GUIDE: &str = include_str!("../../tools/src/guidance/app.md");

#[test]
fn the_guides_own_example_keeps_its_rules() {
    let blocks: Vec<&str> = GUIDE.split("```html\n").skip(1).map(|b| b.split("```").next().unwrap()).collect();
    let page = blocks.iter().find(|b| b.starts_with("<!doctype html>")).expect("a whole page in the guide");
    assert!(page.lines().count() <= 70, "{}", page.lines().count());
    loads_the_library_first("the guide's example", page);
    keeps_the_rules("the guide's example", page);
    let js = page.split("<script>").nth(1).unwrap().split("</script>").next().unwrap();
    uses_only_the_library("the guide's example", js);
    assert!(js.contains("S.suggest(") && js.contains("createElementNS"));
}
