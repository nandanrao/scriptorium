//! The format-independent screenplay model.
//!
//! A [`Script`] is an ordered list of [`Paragraph`]s. Each paragraph has a
//! stable [`ParaId`], an element kind (Action, Dialogue…) and styled text spans.
//! File formats (FDX, Fountain…) live in their own crates and convert to and
//! from this model; review and translation refer to paragraphs by id.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Stable identity of a paragraph. Review proposals and translations point at
/// paragraphs by id, so an id must survive edits to the paragraph's text.
/// Importers assign ids; a sidecar file keeps them across sessions (phase 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ParaId(pub u64);

/// The screenplay element a paragraph is. `Other` keeps custom types
/// (e.g. Final Draft's "End of Act") without losing their name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ElementKind {
    SceneHeading,
    Action,
    Character,
    Parenthetical,
    Dialogue,
    Transition,
    Shot,
    General,
    Other(String),
}

impl ElementKind {
    /// Parse the conventional name ("Scene Heading", "Dialogue"…).
    pub fn from_name(name: &str) -> Self {
        match name {
            "Scene Heading" => Self::SceneHeading,
            "Action" => Self::Action,
            "Character" => Self::Character,
            "Parenthetical" => Self::Parenthetical,
            "Dialogue" => Self::Dialogue,
            "Transition" => Self::Transition,
            "Shot" => Self::Shot,
            "General" => Self::General,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::SceneHeading => "Scene Heading",
            Self::Action => "Action",
            Self::Character => "Character",
            Self::Parenthetical => "Parenthetical",
            Self::Dialogue => "Dialogue",
            Self::Transition => "Transition",
            Self::Shot => "Shot",
            Self::General => "General",
            Self::Other(s) => s,
        }
    }

    /// An editable element name, ignoring case, spaces and dashes ("scene
    /// heading", "SceneHeading", "scene-heading"). `None` for anything else.
    pub fn parse_editable(s: &str) -> Option<Self> {
        let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let want = norm(s);
        [
            Self::SceneHeading,
            Self::Action,
            Self::Character,
            Self::Parenthetical,
            Self::Dialogue,
            Self::Transition,
            Self::Shot,
            Self::General,
        ]
        .into_iter()
        .find(|k| norm(k.name()) == want)
    }

    /// Element kinds whose text a writer or reviewer normally edits.
    pub fn is_editable(&self) -> bool {
        !matches!(self, Self::Other(_))
    }
}

/// Character styling that is meaningful across formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paragraph {
    pub id: ParaId,
    pub kind: ElementKind,
    pub spans: Vec<Span>,
}

impl Paragraph {
    /// A paragraph of plain, unstyled text.
    pub fn plain(id: ParaId, kind: ElementKind, text: impl Into<String>) -> Self {
        Self { id, kind, spans: vec![Span { text: text.into(), style: Style::default() }] }
    }

    /// The paragraph's text with styling removed.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    pub paragraphs: Vec<Paragraph>,
    /// Paragraphs whose text may be edited but which allow no structural edits
    /// (no adding lines among them, removing, splitting, joining or retyping),
    /// because the file format can't write such a change back: the lines of
    /// one Final Draft dual dialogue block. Maps each such paragraph to its
    /// group (e.g. which dual dialogue block, numbered in file order), so two
    /// adjacent groups can be told apart. Filled by importers; empty for
    /// scripts with no such paragraphs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fixed: BTreeMap<ParaId, u32>,
    /// Every id below this has been used for this script (by the file it came
    /// from, or by edits since folded in by [`crate::Document::rebase`]), so a
    /// fresh id is never below it, even when no paragraph has that id now: an
    /// exporter would take a reused id for the original paragraph. 0 = only
    /// the live ids are known to be used.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id_floor: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Script {
    pub fn get(&self, id: ParaId) -> Option<&Paragraph> {
        self.paragraphs.iter().find(|p| p.id == id)
    }

    pub fn position(&self, id: ParaId) -> Option<usize> {
        self.paragraphs.iter().position(|p| p.id == id)
    }

    /// Whether paragraph `id` takes text edits only (see [`Script::fixed`]).
    pub fn is_fixed(&self, id: ParaId) -> bool {
        self.fixed.contains_key(&id)
    }

    /// The fixed group paragraph `id` belongs to, if any (see [`Script::fixed`]).
    pub fn fixed_group(&self, id: ParaId) -> Option<u32> {
        self.fixed.get(&id).copied()
    }
}

#[cfg(test)]
mod kind_tests {
    use super::ElementKind;

    #[test]
    fn parse_editable_ignores_case_and_separators() {
        assert_eq!(ElementKind::parse_editable("scene-heading"), Some(ElementKind::SceneHeading));
        assert_eq!(ElementKind::parse_editable("DIALOGUE"), Some(ElementKind::Dialogue));
        assert_eq!(ElementKind::parse_editable("Banana"), None);
        assert_eq!(ElementKind::parse_editable(""), None);
    }
}
