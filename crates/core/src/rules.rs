//! Screenplay conventions for element flow: what Enter and Tab do.
//!
//! Kept in one small module so it is easy to find and change. These follow
//! Final Draft's defaults closely enough for a first version.

use crate::model::ElementKind;

/// Element for the new paragraph when Enter is pressed at the end of `kind`.
pub fn next_kind(kind: &ElementKind) -> ElementKind {
    use ElementKind::*;
    match kind {
        SceneHeading => Action,
        Action => Action,
        Character => Dialogue,
        Parenthetical => Dialogue,
        Dialogue => Action,
        Transition => SceneHeading,
        Shot => Action,
        General => General,
        Other(_) => Action,
    }
}

/// Order that Tab (forward) and Shift-Tab (backward) cycle through.
pub const CYCLE: &[ElementKind] = &[
    ElementKind::Action,
    ElementKind::Character,
    ElementKind::Dialogue,
    ElementKind::Parenthetical,
    ElementKind::Transition,
    ElementKind::SceneHeading,
    ElementKind::Shot,
];

pub fn cycle_kind(kind: &ElementKind, forward: bool) -> ElementKind {
    let n = CYCLE.len();
    match CYCLE.iter().position(|k| k == kind) {
        Some(i) => CYCLE[if forward { (i + 1) % n } else { (i + n - 1) % n }].clone(),
        None => CYCLE[0].clone(),
    }
}

/// Element kinds shown in capitals on the page.
pub fn is_caps(kind: &ElementKind) -> bool {
    matches!(kind, ElementKind::SceneHeading | ElementKind::Character | ElementKind::Transition | ElementKind::Shot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_round_trips() {
        for k in CYCLE {
            assert_eq!(&cycle_kind(&cycle_kind(k, true), false), k);
        }
        assert_eq!(cycle_kind(&ElementKind::Other("End of Act".into()), true), ElementKind::Action);
    }
}
