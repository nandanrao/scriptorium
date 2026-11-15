//! The grammar of review decision values: the one place they are built and
//! parsed. Decisions are stored as document metadata under
//! `review/<review id>/<entry>` (see `session.rs`); the session-file sync
//! renumbers the paragraph ids they name (see `sessionfile::sync::Renumber`).
//!
//! ```text
//! value  = decision [ "@" ids [ "=>" now ] ]
//! decision = "accepted:" k | "rejected" | "rewritten"   (anything else reads as rewritten)
//! now    = ids | "after:" id | "start"
//! ids    = id { "," id }
//! ```
//!
//! `ids` after `@` are the paragraphs the decision was made on (a line
//! suggestion names one); `now` is what a block decision left in their place:
//! the paragraphs it wrote, or, when it removed every line, the paragraph it
//! sits after (or the start of the script). Values without `@` were written
//! before paragraphs were recorded (and appear in the rebuilt starting point
//! of a version 1 session) and apply to whatever paragraph the item is on.

use scriptorium_core::ParaId;

/// What a decision value records, without the paragraphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stored {
    /// `accepted:<k>`: option `k` of the item.
    Accepted(usize),
    /// `rejected`: kept as it was.
    Rejected,
    /// `rewritten`, or anything else that isn't one of the above.
    Rewritten,
}

/// What a block decision left in place of the lines it was made on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Now {
    /// The paragraphs it wrote.
    Ids(Vec<ParaId>),
    /// It removed every line; the paragraph they were after.
    After(ParaId),
    /// It removed every line, which started the script.
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MetaValue {
    pub decision: Stored,
    /// The paragraphs the decision was made on; `None` in legacy values
    /// (no `@`). Never empty when `Some`.
    pub paras: Option<Vec<ParaId>>,
    /// `=>` part; only with `paras`.
    pub now: Option<Now>,
}

fn parse_ids(list: &str) -> Option<Vec<ParaId>> {
    list.split(',').map(|n| n.parse::<u64>().ok().map(ParaId)).collect()
}

fn write_ids(ids: &[ParaId]) -> String {
    ids.iter().map(|p| p.0.to_string()).collect::<Vec<_>>().join(",")
}

impl MetaValue {
    /// A line decision: made on `para` (`None`: the legacy form, no `@`).
    pub fn line(decision: Stored, para: Option<ParaId>) -> Self {
        Self { decision, paras: para.map(|p| vec![p]), now: None }
    }

    /// `None` for a value whose paragraph parts aren't paragraph ids.
    pub fn parse(value: &str) -> Option<Self> {
        let (d, paras, now) = match value.split_once('@') {
            None => (value, None, None),
            Some((d, rest)) => {
                let (before, after) = match rest.split_once("=>") {
                    None => (rest, None),
                    Some((b, a)) => (b, Some(a)),
                };
                let now = match after {
                    None => None,
                    Some("start") => Some(Now::Start),
                    Some(a) => Some(match a.strip_prefix("after:") {
                        Some(id) => Now::After(ParaId(id.parse::<u64>().ok()?)),
                        None => Now::Ids(parse_ids(a)?),
                    }),
                };
                (d, Some(parse_ids(before)?), now)
            }
        };
        let decision = match d {
            "rejected" => Stored::Rejected,
            d => match d.strip_prefix("accepted:").and_then(|k| k.parse::<usize>().ok()) {
                Some(k) => Stored::Accepted(k),
                None => Stored::Rewritten,
            },
        };
        Some(Self { decision, paras, now })
    }

    /// The decision in `value` if it was made on `para` (its first paragraph,
    /// the one a line item sits on), or doesn't say.
    pub fn on(value: &str, para: ParaId) -> Option<Stored> {
        Self::parse(value).filter(|v| v.paras.as_ref().is_none_or(|ps| ps.first() == Some(&para))).map(|v| v.decision)
    }

    pub fn to_value(&self) -> String {
        let mut out = match self.decision {
            Stored::Accepted(k) => format!("accepted:{k}"),
            Stored::Rejected => "rejected".to_string(),
            Stored::Rewritten => "rewritten".to_string(),
        };
        if let Some(ps) = &self.paras {
            out.push('@');
            out.push_str(&write_ids(ps));
            match &self.now {
                None => {}
                Some(Now::Ids(ids)) => out.push_str(&format!("=>{}", write_ids(ids))),
                Some(Now::After(id)) => out.push_str(&format!("=>after:{}", id.0)),
                Some(Now::Start) => out.push_str("=>start"),
            }
        }
        out
    }

    /// Every paragraph id it names passed through `f`.
    pub fn map_ids(mut self, f: impl Fn(ParaId) -> ParaId) -> Self {
        if let Some(ps) = &mut self.paras {
            ps.iter_mut().for_each(|p| *p = f(*p));
        }
        match &mut self.now {
            Some(Now::Ids(ids)) => ids.iter_mut().for_each(|p| *p = f(*p)),
            Some(Now::After(id)) => *id = f(*id),
            Some(Now::Start) | None => {}
        }
        self
    }
}

/// `value` with the paragraph ids it names passed through `f`. A value that
/// names none (legacy, no `@`) comes back exactly as it was; one that doesn't
/// parse gives `None`. A parsed value is rewritten in its canonical form (an
/// unknown decision word reads, and is written back, as `rewritten`).
pub(crate) fn renumber(value: &str, f: impl Fn(ParaId) -> ParaId) -> Option<String> {
    if !value.contains('@') {
        return Some(value.to_string());
    }
    Some(MetaValue::parse(value)?.map_ids(f).to_value())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ns: &[u64]) -> Vec<ParaId> {
        ns.iter().copied().map(ParaId).collect()
    }

    #[test]
    fn every_form_round_trips() {
        let v = |decision, paras: Option<&[u64]>, now| MetaValue { decision, paras: paras.map(ids), now };
        for (text, value) in [
            ("accepted:2@7", v(Stored::Accepted(2), Some(&[7]), None)),
            ("rejected@0", v(Stored::Rejected, Some(&[0]), None)),
            ("rewritten@31", v(Stored::Rewritten, Some(&[31]), None)),
            ("rejected@9,10,11", v(Stored::Rejected, Some(&[9, 10, 11]), None)),
            ("accepted:0@4,5=>4,20", v(Stored::Accepted(0), Some(&[4, 5]), Some(Now::Ids(ids(&[4, 20]))))),
            ("rewritten@3=>8", v(Stored::Rewritten, Some(&[3]), Some(Now::Ids(ids(&[8]))))),
            ("accepted:1@11,12=>after:10", v(Stored::Accepted(1), Some(&[11, 12]), Some(Now::After(ParaId(10))))),
            ("accepted:1@0=>start", v(Stored::Accepted(1), Some(&[0]), Some(Now::Start))),
            ("rejected@5=>start", v(Stored::Rejected, Some(&[5]), Some(Now::Start))),
            ("accepted:1", v(Stored::Accepted(1), None, None)),
            ("rejected", v(Stored::Rejected, None, None)),
            ("rewritten", v(Stored::Rewritten, None, None)),
        ] {
            let parsed = MetaValue::parse(text).unwrap();
            assert_eq!(parsed, value, "{text}");
            assert_eq!(parsed.to_value(), text);
        }
    }

    #[test]
    fn odd_values() {
        assert_eq!(MetaValue::parse("accepted:x").unwrap().decision, Stored::Rewritten);
        for bad in ["rejected@nope", "rejected@", "rejected@1,", "accepted:0@1=>after:x", "accepted:0@1=>x", "a@1=>"] {
            assert_eq!(MetaValue::parse(bad), None, "{bad}");
        }
        assert_eq!(MetaValue::on("rejected@3", ParaId(3)), Some(Stored::Rejected));
        assert_eq!(MetaValue::on("rejected@3", ParaId(4)), None);
        assert_eq!(MetaValue::on("rejected", ParaId(4)), Some(Stored::Rejected));
        assert_eq!(MetaValue::on("accepted:0@3,4=>5", ParaId(3)), Some(Stored::Accepted(0)));
        assert_eq!(MetaValue::line(Stored::Rejected, Some(ParaId(2))).to_value(), "rejected@2");
        assert_eq!(MetaValue::line(Stored::Rejected, None).to_value(), "rejected");
    }

    #[test]
    fn renumbering_maps_every_id() {
        let f = |p: ParaId| if p.0 >= 10 { ParaId(p.0 + 10) } else { p };
        assert_eq!(renumber("accepted:1@11,12=>after:10", f).as_deref(), Some("accepted:1@21,22=>after:20"));
        assert_eq!(renumber("rejected@9,10=>4,11", f).as_deref(), Some("rejected@9,20=>4,21"));
        assert_eq!(renumber("rewritten=>12", f).as_deref(), Some("rewritten=>12"), "legacy kept verbatim");
        assert_eq!(renumber("odd@x", f), None);
    }
}
