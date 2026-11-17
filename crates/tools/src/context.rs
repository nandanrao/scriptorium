//! `where_is_writer` (plan "agent connect", C3): what the writer has open in
//! the app and where they are in it, for an outside agent. The app fills a
//! [`ViewContext`] from its open script view; [`text`] says it in plain
//! words. On the files there is no writer to place ([`ON_FILES`]).
//!
//! Pure: the app hands in values, the helpers here ([`scene_of`],
//! [`selected`]) read a [`Document`], and nothing is looked up elsewhere.
//! The agent panel's own conversation is never part of it.

use scriptorium_core::outline::annotate;
use scriptorium_core::{Document, ParaId, Selection};
use serde_json::json;

use crate::catalogue::{ToolSpec, spec};

/// The tool's name.
pub const NAME: &str = "where_is_writer";

/// `where_is_writer` run on the files (no app to ask).
pub const ON_FILES: &str = "Scriptorium isn't open; nothing is selected. Work on the script's path.";

/// The most selected text quoted; the rest is left to the reading tools.
pub const MAX_QUOTE: usize = 2000;

/// Where the writer is in the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewContext {
    /// The open script's absolute path.
    pub script: String,
    /// The pair of versions the script is in, when one loaded.
    pub pair: Option<PairPlace>,
    /// What the main view shows.
    pub view: Showing,
    /// The suggestion on the card (a review showing, one not yet finished).
    pub card: Option<Card>,
    /// The selection, or the cursor's line.
    pub selection: Option<Selected>,
    /// The scene the writer is in (the card's line while a review shows,
    /// else the cursor's); `None` before the first scene heading.
    pub scene: Option<Scene>,
    /// The first and last lines on screen; `None` before the view is drawn.
    pub visible: Option<(ParaId, ParaId)>,
    /// The script has changes in the app not yet written to an .fdx.
    pub unwritten: bool,
}

/// The script's place in its pair of versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairPlace {
    /// This script's label in the pair ("Spanish", or its file's stem).
    pub this: String,
    /// The other member's label.
    pub other: String,
    /// The other member's file name.
    pub other_file: String,
    /// This script is the version (the side translated into).
    pub this_is_version: bool,
    /// The other member is lined up beside it on screen.
    pub lined_up: bool,
}

/// What the main view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Showing {
    Script,
    /// A review, by the name `list_reviews` shows.
    Review(String),
}

/// The suggestion on the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// Its number, as `list_suggestions` shows it.
    pub number: usize,
    /// The line it is shown at.
    pub para: ParaId,
}

/// The writer's selection: lines and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    /// The first and last lines it touches, in script order.
    pub first: ParaId,
    pub last: ParaId,
    /// The selected text (lines joined with newlines); for a collapsed
    /// selection, the cursor's whole line.
    pub text: String,
    /// Nothing selected: just the cursor.
    pub collapsed: bool,
}

/// A scene, numbered as `read_scene` numbers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    pub number: usize,
    pub heading: String,
}

/// The scene holding `para` in `doc`, numbered as `read_scene` does (1 =
/// the first heading); `None` before the first heading, or for a line not
/// in `doc`.
pub fn scene_of(doc: &Document, para: ParaId) -> Option<Scene> {
    let ix = doc.index_of(para)?;
    let cx = annotate(doc.script());
    let number = cx[..=ix].iter().filter(|c| c.heading).count();
    (number > 0).then(|| Scene { number, heading: cx[ix].scene.trim().to_string() })
}

/// `sel` in `doc`, as [`Selected`]. `None` when an end isn't in `doc`.
pub fn selected(doc: &Document, sel: Selection) -> Option<Selected> {
    doc.index_of(sel.anchor.para)?;
    doc.index_of(sel.head.para)?;
    let (a, b) = doc.ordered(sel);
    let collapsed = sel.is_collapsed();
    let text = if collapsed { doc.paragraph(a.para)?.text() } else { doc.text_in(sel) };
    Some(Selected { first: a.para, last: b.para, text, collapsed })
}

/// The tool's spec: no arguments.
pub fn where_spec() -> ToolSpec {
    spec(
        NAME,
        "Where the writer is in Scriptorium: the script open in the app (its path), the pair of versions it is in, \
         whether the script or a review is showing and the suggestion on the card, the selected text (or the \
         cursor's line), the scene, the lines on screen, and whether there are changes not yet written. Call it \
         first, and again when the writer says \"this scene\" or \"here\". Read-only; takes no arguments.",
        json!({}),
        &[],
    )
}

/// `ctx` in plain words, one fact a line.
pub fn text(ctx: &ViewContext) -> String {
    let mut out = vec![format!("Open in Scriptorium: {} (leave `script` out to work on it).", ctx.script)];
    if let Some(p) = &ctx.pair {
        let side = if p.this_is_version {
            format!("this is the version ({}); the original is {} ({})", p.this, p.other, p.other_file)
        } else {
            format!("this is the original ({}); its version is {} ({})", p.this, p.other, p.other_file)
        };
        let lined = if p.lined_up { "lined up beside it on screen" } else { "not lined up on screen" };
        out.push(format!("Versions: {side}, {lined}."));
    }
    out.push(match (&ctx.view, &ctx.card) {
        (Showing::Script, _) => "Showing: the script.".to_string(),
        (Showing::Review(name), Some(c)) => {
            format!("Showing: the review {name}; the card shows suggestion #{} (at ¶{}).", c.number, c.para.0)
        }
        (Showing::Review(name), None) => {
            format!("Showing: the review {name}; every suggestion in it is decided (no card).")
        }
    });
    if let Some(s) = &ctx.selection {
        let lines =
            if s.first == s.last { format!("¶{}", s.first.0) } else { format!("¶{} to ¶{}", s.first.0, s.last.0) };
        let what =
            if s.collapsed { format!("Cursor: on {lines}, which reads:") } else { format!("Selected: {lines}:") };
        out.push(format!("{what}\n{}", quote(&s.text)));
    }
    out.push(match &ctx.scene {
        Some(s) => format!("Scene: {}, {} (read_scene with scene {}).", s.number, s.heading, s.number),
        None => "Scene: none (before the first scene heading).".to_string(),
    });
    out.push(match ctx.visible {
        Some((a, b)) => format!("On screen: ¶{} to ¶{}.", a.0, b.0),
        None => "On screen: not drawn yet.".to_string(),
    });
    out.push(
        if ctx.unwritten {
            "Not yet written: the script has changes in Scriptorium that aren't in an .fdx yet."
        } else {
            "Written: no changes are waiting to be written to an .fdx."
        }
        .to_string(),
    );
    out.join("\n")
}

/// `text` indented as a quote, cut at [`MAX_QUOTE`] characters.
fn quote(text: &str) -> String {
    let n = text.chars().count();
    let (shown, more) = if n > MAX_QUOTE {
        let cut: String = text.chars().take(MAX_QUOTE).collect();
        (cut, format!("\n  … ({} more characters: read the lines for the rest)", n - MAX_QUOTE))
    } else {
        (text.to_string(), String::new())
    };
    let body = if shown.is_empty() { "(empty line)".to_string() } else { shown };
    let lines: Vec<String> = body.lines().map(|l| format!("  {l}")).collect();
    format!("{}{more}", if lines.is_empty() { "  ".to_string() } else { lines.join("\n") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use scriptorium_core::{ElementKind, Paragraph, Pos, Script};

    fn doc() -> Document {
        let p = |id: u64, kind: ElementKind, text: &str| Paragraph::plain(ParaId(id), kind, text);
        Document::new(Script {
            paragraphs: vec![
                p(1, ElementKind::Action, "Black."),
                p(2, ElementKind::SceneHeading, "INT. KITCHEN - DAY"),
                p(3, ElementKind::Action, "Laura cooks."),
                p(4, ElementKind::Character, "LAURA"),
                p(5, ElementKind::Dialogue, "Hungry?"),
                p(6, ElementKind::SceneHeading, "EXT. GARDEN - NIGHT"),
                p(7, ElementKind::Action, "Rain."),
            ],
            ..Script::default()
        })
    }

    fn ctx() -> ViewContext {
        ViewContext {
            script: "/films/PURPLE LIST.fdx".into(),
            pair: None,
            view: Showing::Script,
            card: None,
            selection: None,
            scene: None,
            visible: None,
            unwritten: false,
        }
    }

    #[test]
    fn scenes_are_numbered_as_read_scene_numbers_them() {
        let d = doc();
        assert_eq!(scene_of(&d, ParaId(1)), None, "before the first heading");
        assert_eq!(scene_of(&d, ParaId(2)), Some(Scene { number: 1, heading: "INT. KITCHEN - DAY".into() }));
        assert_eq!(scene_of(&d, ParaId(5)).unwrap().number, 1);
        assert_eq!(scene_of(&d, ParaId(7)), Some(Scene { number: 2, heading: "EXT. GARDEN - NIGHT".into() }));
        assert_eq!(scene_of(&d, ParaId(99)), None);
    }

    #[test]
    fn a_selection_across_lines_is_in_script_order() {
        let d = doc();
        // Selected upwards: from "Hungry?" back into "Laura cooks.".
        let sel = Selection { anchor: Pos::new(ParaId(5), 3), head: Pos::new(ParaId(3), 6) };
        let s = selected(&d, sel).unwrap();
        assert_eq!((s.first, s.last, s.collapsed), (ParaId(3), ParaId(5), false));
        assert_eq!(s.text, "cooks.\nLAURA\nHun");
        let t = text(&ViewContext { selection: Some(s), ..ctx() });
        assert!(t.contains("Selected: ¶3 to ¶5:\n  cooks.\n  LAURA\n  Hun"), "{t}");
    }

    #[test]
    fn a_collapsed_selection_is_the_cursors_line() {
        let d = doc();
        let s = selected(&d, Selection::cursor(Pos::new(ParaId(3), 2))).unwrap();
        assert_eq!((s.first, s.last, s.collapsed), (ParaId(3), ParaId(3), true));
        assert_eq!(s.text, "Laura cooks.");
        let t = text(&ViewContext { selection: Some(s), ..ctx() });
        assert!(t.contains("Cursor: on ¶3, which reads:\n  Laura cooks."), "{t}");
        // A line gone from the document: nothing to say.
        assert_eq!(selected(&d, Selection::cursor(Pos::new(ParaId(42), 0))), None);
    }

    #[test]
    fn the_plain_script_says_only_what_there_is() {
        let t = text(&ctx());
        assert_eq!(
            t,
            "Open in Scriptorium: /films/PURPLE LIST.fdx (leave `script` out to work on it).\n\
             Showing: the script.\n\
             Scene: none (before the first scene heading).\n\
             On screen: not drawn yet.\n\
             Written: no changes are waiting to be written to an .fdx."
        );
    }

    #[test]
    fn a_review_a_pair_a_scene_and_the_screen() {
        let c = ViewContext {
            pair: Some(PairPlace {
                this: "Spanish".into(),
                other: "English".into(),
                other_file: "PURPLE LIST.fdx".into(),
                this_is_version: true,
                lined_up: true,
            }),
            view: Showing::Review("dialogue-pass".into()),
            card: Some(Card { number: 3, para: ParaId(120) }),
            scene: Some(Scene { number: 4, heading: "INT. KITCHEN - DAY".into() }),
            visible: Some((ParaId(100), ParaId(140))),
            unwritten: true,
            ..ctx()
        };
        let t = text(&c);
        for want in [
            "Versions: this is the version (Spanish); the original is English (PURPLE LIST.fdx), lined up beside it on screen.",
            "Showing: the review dialogue-pass; the card shows suggestion #3 (at ¶120).",
            "Scene: 4, INT. KITCHEN - DAY (read_scene with scene 4).",
            "On screen: ¶100 to ¶140.",
            "Not yet written",
        ] {
            assert!(t.contains(want), "{want}\n{t}");
        }
        // The original's side, not lined up; a finished review.
        let c = ViewContext {
            pair: Some(PairPlace {
                this_is_version: false,
                lined_up: false,
                this: "English".into(),
                ..c.pair.unwrap()
            }),
            card: None,
            ..c
        };
        let t = text(&c);
        assert!(t.contains("this is the original (English); its version is English"), "{t}");
        assert!(t.contains("not lined up on screen"), "{t}");
        assert!(t.contains("every suggestion in it is decided (no card)"), "{t}");
    }

    #[test]
    fn a_long_selection_is_cut() {
        let long = "x".repeat(MAX_QUOTE + 10);
        let s = Selected { first: ParaId(1), last: ParaId(1), text: long, collapsed: false };
        let t = text(&ViewContext { selection: Some(s), ..ctx() });
        assert!(t.contains("… (10 more characters"), "{t}");
        // An empty line is said, not left blank.
        let s = Selected { first: ParaId(1), last: ParaId(1), text: String::new(), collapsed: true };
        assert!(text(&ViewContext { selection: Some(s), ..ctx() }).contains("  (empty line)"));
    }
}
