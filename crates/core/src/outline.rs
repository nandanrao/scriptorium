//! Derived structure: which scene each paragraph is in, and who is speaking.

use std::sync::LazyLock;

use regex::Regex;

use crate::model::{ElementKind, Paragraph, Script};

static SCENE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(INT|EXT|INT\./EXT|I/E)[. ]").unwrap());

/// Context derived for one paragraph by reading the script top to bottom.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParaContext {
    /// True for scene headings, including ones typed as Action ("INT. KITCHEN").
    pub heading: bool,
    /// The scene heading text this paragraph falls under ("" before the first).
    pub scene: String,
    /// For Dialogue and Parenthetical: the character speaking. Otherwise "".
    pub speaker: String,
}

pub fn is_heading(p: &Paragraph) -> bool {
    p.kind == ElementKind::SceneHeading || SCENE_RE.is_match(&p.text())
}

/// One [`ParaContext`] per paragraph, in order.
pub fn annotate(script: &Script) -> Vec<ParaContext> {
    let mut scene = String::new();
    let mut speaker = String::new();
    script
        .paragraphs
        .iter()
        .map(|p| {
            let text = p.text();
            let heading = is_heading(p);
            if heading {
                scene = text.trim().to_string();
            }
            if p.kind == ElementKind::Character {
                speaker = text.trim().to_string();
            }
            let speaks = matches!(p.kind, ElementKind::Dialogue | ElementKind::Parenthetical);
            ParaContext { heading, scene: scene.clone(), speaker: if speaks { speaker.clone() } else { String::new() } }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParaId, Paragraph};

    fn script(paras: &[(&str, &str)]) -> Script {
        Script {
            paragraphs: paras
                .iter()
                .enumerate()
                .map(|(i, (k, t))| Paragraph::plain(ParaId(i as u64), ElementKind::from_name(k), *t))
                .collect(),
        }
    }

    #[test]
    fn scenes_and_speakers() {
        let s = script(&[
            ("Scene Heading", "EXT. FARM HOUSE - NIGHT"),
            ("Action", "They play cards."),
            ("Character", "CESAR"),
            ("Dialogue", "Hello."),
            ("Parenthetical", "(smiling)"),
            ("Action", "INT. KITCHEN - DAY"),
            ("Action", "Rain."),
        ]);
        let ctx = annotate(&s);
        assert_eq!(ctx[1].scene, "EXT. FARM HOUSE - NIGHT");
        assert_eq!(ctx[3].speaker, "CESAR");
        assert_eq!(ctx[4].speaker, "CESAR");
        assert_eq!(ctx[1].speaker, "");
        assert!(ctx[5].heading, "scene heading typed as Action");
        assert_eq!(ctx[6].scene, "INT. KITCHEN - DAY");
        let headings: Vec<bool> = ctx.iter().map(|c| c.heading).collect();
        assert_eq!(headings, [true, false, false, false, false, true, false]);
    }
}
