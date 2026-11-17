//! How long numbering and line changes take on a long, much-edited script:
//! 3000 paragraphs, 500 suggestions, 10 000 edits (typing, splits, joins,
//! removals, undos). The time limit is checked in release builds only
//! (`cargo test --release -p scriptorium-tools --test perf -- --nocapture`
//! prints the timings).

use std::time::{Duration, Instant};

use scriptorium_core::{Document, ElementKind, Group, ParaId, Paragraph, Pos, Script, Selection};
use scriptorium_review::{Review, Session};
use scriptorium_tools::item_numbers;

const PARAS: u64 = 3000;
const ITEMS: u64 = 500;
const EDITS: usize = 10_000;

fn script() -> Script {
    let paragraphs = (0..PARAS)
        .map(|i| Paragraph::plain(ParaId(i), ElementKind::Action, format!("Line number {i} of the film.")))
        .collect();
    Script { paragraphs, ..Script::default() }
}

fn review_text() -> String {
    let mut text = String::from("SCRIPT REVIEW v1\nId: perf\n");
    for k in 0..ITEMS {
        let p = k * (PARAS / ITEMS);
        text += &format!("---\nPARA: {p}\nBEFORE: Line number {p} of the film.\nAFTER: Line {p} of the film.\n");
    }
    text
}

/// A small deterministic random number generator (no dependencies).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn edited() -> Document {
    let mut doc = Document::new(script());
    let mut rng = Rng(7);
    for _ in 0..EDITS {
        let n = doc.paragraphs().len();
        let p = &doc.paragraphs()[rng.below(n)];
        let (id, len) = (p.id, p.text().len());
        let at = |o: usize| Selection::cursor(Pos::new(id, o));
        let _ = match rng.below(100) {
            0..=79 => doc.insert_text(at(rng.below(len + 1)), "x").map(drop),
            80..=86 => doc.enter(at(rng.below(len + 1))).map(drop),
            87..=91 => doc.delete_backward(at(0), false).map(drop),
            92..=94 if n > 1 => doc
                .edit(at(0), Group::Other, |e| {
                    e.remove_para(id)?;
                    Ok((Selection::cursor(Pos::new(ParaId(0), 0)), ()))
                })
                .map(drop),
            _ => {
                doc.undo();
                Ok(())
            }
        };
    }
    assert!(doc.log().len() >= EDITS * 9 / 10, "most edits applied");
    doc
}

#[test]
fn numbers_and_line_changes_are_fast_on_a_long_edited_script() {
    let original = script();
    let doc = edited();
    let review = Review::open_live("perf.review.txt", &review_text(), &original, &doc, |_| false, "", None);
    assert_eq!(review.items.len() as u64, ITEMS);
    let session = Session::new(vec![review]);
    let items = &session.reviews[0].items;

    let t = Instant::now();
    let changes: Vec<_> = (0..items.len()).map(|i| session.line_change(&doc, 0, i)).collect();
    let line_changes = t.elapsed();
    let t = Instant::now();
    let numbers = item_numbers(&doc, items);
    let numbering = t.elapsed();
    let t = Instant::now();
    let outdated = (0..items.len()).filter(|&i| session.is_outdated(&doc, 0, i)).count();
    let marks = t.elapsed();
    println!(
        "{} paragraphs, {} log events: line changes {line_changes:?} ({} changed), numbers {numbering:?}, \
         is_outdated for all {marks:?} ({outdated} outdated)",
        doc.paragraphs().len(),
        doc.log().len(),
        changes.iter().filter(|c| c.is_some()).count()
    );
    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (1..=items.len()).collect::<Vec<_>>());
    if !cfg!(debug_assertions) {
        assert!(line_changes + numbering < Duration::from_millis(10), "{line_changes:?} + {numbering:?}");
        assert!(marks < Duration::from_millis(10), "{marks:?}");
    }
}
