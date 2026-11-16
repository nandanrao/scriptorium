//! Tool-row labels and usage text, shared by the panel and the CLI's stderr.
//!
//! A row shows a *running* label while the call is in flight and a *done*
//! label afterwards. Labels are built from the call's arguments and its
//! result text (both as the `scriptorium-tools` catalogue defines them); a
//! label never fails: anything it can't read falls back to a plainer wording.

use scriptorium_agent::Usage;
use serde_json::Value;

/// How a thread's own review is named when it has no name to show, and in
/// tool results; the same wording everywhere.
pub const OWN_REVIEW: &str = "this conversation's review";

/// The row of a call that was never run because the run was stopped.
pub const NOT_RUN: &str = "Not run (stopped)";

/// Longest scene heading shown in a label (characters).
const HEADING: usize = 40;

/// The running label of call `tool` with arguments `args` (`Null` while they
/// are still streaming). `bytes_so_far` is the streamed argument size, shown
/// for batches (`Event::ToolArgs`).
pub fn running(tool: &str, args: &Value, bytes_so_far: Option<usize>) -> String {
    match tool {
        "script_info" => "Looking at the script…".into(),
        "read_scene" => match scene_ref(args) {
            Some(Scene::Number(n)) => format!("Reading scene {n}…"),
            Some(Scene::Heading(h)) => format!("Reading the scene matching \"{h}\"…"),
            Some(Scene::Around(id)) => format!("Reading the scene around ¶{id}…"),
            None => "Reading a scene…".into(),
        },
        "read_script" => "Reading the whole script…".into(),
        "list_paragraphs" => "Finding lines…".into(),
        "list_reviews" => "Looking at the reviews…".into(),
        "list_suggestions" => "Looking at the suggestions…".into(),
        "suggest_change" => "Writing a suggestion…".into(),
        "suggest_changes" => {
            let size = bytes_so_far.map(|b| format!(" ({})", bytes_text(b))).unwrap_or_default();
            match batch_len(args) {
                Some(n) => format!("Writing {} {}…{size}", n, plural(n, "suggestion", "suggestions")),
                None => format!("Writing suggestions…{size}"),
            }
        }
        other => format!("Running {other}…"),
    }
}

/// The done label of a call that returned `result`. `review` names the review
/// the call used or wrote to (`None`: the thread's own, shown as
/// [`OWN_REVIEW`]).
pub fn done(tool: &str, args: &Value, result: &str, review: Option<&str>) -> String {
    let review_name = review.unwrap_or(OWN_REVIEW);
    match tool {
        "script_info" => "Looked at the script summary".into(),
        "read_scene" => read_scene_done(args, result),
        "read_script" => match script_part(result) {
            Some(covers) => format!("Read the whole script ({covers})"),
            None => "Read the whole script".into(),
        },
        "list_paragraphs" => list_paragraphs_done(args, result),
        "list_reviews" => "Listed reviews".into(),
        "list_suggestions" => {
            let name = args.get("review").and_then(Value::as_str).unwrap_or(review_name);
            format!("Listed suggestions in {name}")
        }
        "suggest_change" => format!("{} → {review_name}", change_text(args)),
        "suggest_changes" => {
            let json: Option<Value> = serde_json::from_str(result).ok();
            let count = |key: &str| {
                let v = json.as_ref()?.get(key)?;
                v.as_u64().or_else(|| v.as_array().map(|a| a.len() as u64))
            };
            // Nothing passed validation: the result is a sentence, every item refused.
            let added = count("added").unwrap_or(0);
            let refused =
                count("failed").unwrap_or_else(|| if json.is_some() { 0 } else { batch_len(args).unwrap_or(0) as u64 });
            let mut s = format!("Proposed {added} {}", plural(added as usize, "change", "changes"));
            if added > 0 {
                s.push_str(&format!(" → {review_name}"));
            }
            if refused > 0 {
                s.push_str(&format!(" · {refused} refused"));
            }
            s
        }
        other => format!("Ran {other}"),
    }
}

/// The label of a call that failed; the panel shows it in the warning colour.
/// Plain words for the writer, never the tool's name; `reason` (the tool's own
/// message, written for the model) stays in the row's expandable result.
pub fn failed(tool: &str, _reason: &str) -> String {
    let what = match tool {
        "script_info" => "look at the script summary",
        "read_scene" => "read the scene",
        "read_script" => "read the script",
        "list_paragraphs" => "find lines",
        "list_reviews" => "list reviews",
        "list_suggestions" => "list suggestions",
        "suggest_change" => "write the suggestion",
        "suggest_changes" => "write the suggestions",
        _ => "finish a step",
    };
    format!("Couldn't {what}")
}

enum Scene {
    Number(u64),
    Heading(String),
    Around(u64),
}

fn scene_ref(args: &Value) -> Option<Scene> {
    match args.get("scene") {
        Some(Value::Number(n)) => return n.as_u64().map(Scene::Number),
        Some(Value::String(s)) if !s.trim().is_empty() => {
            let s = s.trim();
            // As the tool reads it: digits are a scene number if there is one.
            return Some(s.parse().map_or_else(|_| Scene::Heading(s.to_string()), Scene::Number));
        }
        _ => {}
    }
    args.get("paragraph_id").and_then(Value::as_u64).map(Scene::Around)
}

fn read_scene_done(args: &Value, result: &str) -> String {
    // "SCENE 12 of 40 (¶a–¶b)\n¶id <heading>" ; "BEFORE SCENE 1 (¶a–¶b)".
    let mut lines = result.lines();
    let head = lines.next().unwrap_or("");
    let number =
        head.strip_prefix("SCENE ").and_then(|r| r.split_whitespace().next()).and_then(|n| n.parse::<u64>().ok());
    let heading = lines
        .next()
        .and_then(|l| l.strip_prefix('¶'))
        .and_then(|l| l.split_once(' '))
        .map(|(_, t)| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| cut(t, HEADING));
    if head.starts_with("BEFORE SCENE") {
        return "Read the lines before scene 1".into();
    }
    let what = match (number, scene_ref(args)) {
        (Some(n), _) | (None, Some(Scene::Number(n))) => format!("scene {n}"),
        _ => "a scene".into(),
    };
    match heading {
        Some(h) => format!("Read {what} ({h})"),
        None => format!("Read {what}"),
    }
}

/// "scenes 3–5 of 9" from `read_script`'s "This part: scenes 3–5 of 9 (…)";
/// `None` for the whole script in one go.
fn script_part(result: &str) -> Option<String> {
    let rest = result.split_once("This part: ")?.1;
    let covers = rest.split(" (").next()?;
    Some(covers.to_string())
}

fn list_paragraphs_done(args: &Value, result: &str) -> String {
    let total = serde_json::from_str::<Value>(result).ok().and_then(|v| v.get("total")?.as_u64());
    let mut filters: Vec<String> = Vec::new();
    let text = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = text("speaker") {
        filters.push(s.to_string());
    }
    if let Some(s) = text("scene") {
        filters.push(if s.chars().all(|c| c.is_ascii_digit()) {
            format!("scene {s}")
        } else {
            format!("scene \"{s}\"")
        });
    }
    if let Some(k) = text("kind") {
        filters.push(k.to_string());
    }
    if let Some(c) = text("contains") {
        filters.push(format!("\"{c}\""));
    }
    let with = if filters.is_empty() { String::new() } else { format!(" ({})", filters.join(", ")) };
    match total {
        Some(n) => format!("Found {n} {}{with}", plural(n as usize, "line", "lines")),
        None => format!("Searched the lines{with}"),
    }
}

/// "Proposed a change to ¶812", of one line, a block, or an insertion.
fn change_text(args: &Value) -> String {
    if let Some(ids) = args.get("paragraph_ids").and_then(Value::as_array) {
        let ids: Vec<u64> = ids.iter().filter_map(Value::as_u64).collect();
        return match ids.as_slice() {
            [] => "Proposed a change".into(),
            [one] => format!("Proposed a change to ¶{one}"),
            [first, .., last] => format!("Proposed a change to ¶{first}–¶{last}"),
        };
    }
    if let Some(id) = args.get("paragraph_id").and_then(Value::as_u64) {
        return format!("Proposed a change to ¶{id}");
    }
    if let Some(id) = args.get("insert_after").and_then(Value::as_u64) {
        return format!("Proposed new lines after ¶{id}");
    }
    "Proposed a change".into()
}

fn batch_len(args: &Value) -> Option<usize> {
    args.get("suggestions").and_then(Value::as_array).map(Vec::len)
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// `s` cut to `max` characters, with "…" when something was cut.
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A byte count as "840 B" or "3.2 kB".
pub fn bytes_text(bytes: usize) -> String {
    if bytes < 1000 { format!("{bytes} B") } else { format!("{:.1} kB", bytes as f64 / 1000.0) }
}

/// A token count as "850", "18.2k" or "1.3M".
pub fn tokens_short(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    // In tenths, rounded half up; a count that rounds to 1000.0k reads 1.0M.
    let k = n.saturating_add(50) / 100;
    if k < 10_000 {
        return format!("{}.{}k", k / 10, k % 10);
    }
    let m = n.saturating_add(50_000) / 100_000;
    format!("{}.{}M", m / 10, m % 10)
}

/// A cost in US dollars: "$0.24", or "<$0.01" for a fraction of a cent.
pub fn cost_text(dollars: f64) -> String {
    if dollars > 0.0 && dollars < 0.005 { "<$0.01".into() } else { format!("${dollars:.2}") }
}

/// "18.2k tokens · $0.24". The cost is `usage.cost` when the provider
/// reported one, else `priced` (computed from the price table by the caller).
///
/// When a reply's counts aren't known (a stopped reply), the totals of the
/// others are a floor: "at least 1.2k tokens · $0.01+", where `priced` is the
/// cost of the replies that are known. A part with nothing known reads
/// "tokens unknown" / "cost unknown".
pub fn usage_text(usage: &Usage, priced: Option<f64>) -> String {
    let tokens = usage.input + usage.output;
    if !usage.known {
        if tokens == 0 {
            return "tokens unknown · cost unknown".into();
        }
        let cost = priced.filter(|c| *c > 0.0).map_or("cost unknown".to_string(), |c| format!("{}+", cost_text(c)));
        return format!("at least {} tokens · {cost}", tokens_short(tokens));
    }
    let cost = usage.cost.or(priced).map_or("cost unknown".to_string(), cost_text);
    format!("{} tokens · {cost}", tokens_short(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(input: u64, output: u64, known: bool, cost: Option<f64>) -> Usage {
        Usage { input, output, cached: 0, cache_write: 0, reasoning: None, known, cost }
    }

    #[test]
    fn running_labels() {
        let n = Value::Null;
        assert_eq!(running("script_info", &n, None), "Looking at the script…");
        assert_eq!(running("read_scene", &json!({"scene": 12}), None), "Reading scene 12…");
        assert_eq!(running("read_scene", &json!({"scene": "12"}), None), "Reading scene 12…");
        assert_eq!(
            running("read_scene", &json!({"scene": "kitchen"}), None),
            "Reading the scene matching \"kitchen\"…"
        );
        assert_eq!(running("read_scene", &json!({"paragraph_id": 7}), None), "Reading the scene around ¶7…");
        assert_eq!(running("read_scene", &n, None), "Reading a scene…");
        assert_eq!(running("read_script", &n, None), "Reading the whole script…");
        assert_eq!(running("list_paragraphs", &n, None), "Finding lines…");
        assert_eq!(running("suggest_change", &n, None), "Writing a suggestion…");
        assert_eq!(running("nope", &n, None), "Running nope…");
    }

    #[test]
    fn running_batch() {
        let two = json!({"suggestions": [{}, {}]});
        assert_eq!(running("suggest_changes", &two, Some(3200)), "Writing 2 suggestions… (3.2 kB)");
        assert_eq!(running("suggest_changes", &json!({"suggestions": [{}]}), None), "Writing 1 suggestion…");
        assert_eq!(running("suggest_changes", &Value::Null, Some(840)), "Writing suggestions… (840 B)");
    }

    #[test]
    fn read_scene() {
        let r = "SCENE 12 of 40 (¶100–¶130)\n¶100 INT. KITCHEN – NIGHT\n¶101 Marta stirs.";
        assert_eq!(done("read_scene", &json!({"scene": 12}), r, None), "Read scene 12 (INT. KITCHEN – NIGHT)");
        // The number comes from the result when the call named the scene by text.
        assert_eq!(done("read_scene", &json!({"scene": "kitchen"}), r, None), "Read scene 12 (INT. KITCHEN – NIGHT)");
        let before = "BEFORE SCENE 1 (¶1–¶3)\n¶1 FADE IN:";
        assert_eq!(done("read_scene", &json!({"scene": 1}), before, None), "Read the lines before scene 1");
        assert_eq!(done("read_scene", &json!({"scene": 3}), "", None), "Read scene 3");
        assert_eq!(done("read_scene", &Value::Null, "", None), "Read a scene");
        let long = format!("SCENE 2 of 3 (¶1–¶2)\n¶1 {}", "A".repeat(80));
        assert!(done("read_scene", &Value::Null, &long, None).ends_with("…)"));
    }

    #[test]
    fn read_script() {
        let whole = "f.fdx: 3 scenes, 9 paragraphs, ≈1k tokens\n\nSCENE 1 of 3";
        assert_eq!(done("read_script", &Value::Null, whole, None), "Read the whole script");
        let part = "f.fdx: 9 scenes in all. This part: scenes 3–5 of 9 (≈4k tokens). More follows: call read_script";
        assert_eq!(done("read_script", &Value::Null, part, None), "Read the whole script (scenes 3–5 of 9)");
    }

    #[test]
    fn list_paragraphs() {
        let args = json!({"speaker": "MARTA", "scene": "12"});
        let r = json!({"total": 14, "offset": 0, "more": false, "paragraphs": []}).to_string();
        assert_eq!(done("list_paragraphs", &args, &r, None), "Found 14 lines (MARTA, scene 12)");
        let one = json!({"total": 1}).to_string();
        assert_eq!(
            done("list_paragraphs", &json!({"contains": "gun", "kind": "Action"}), &one, None),
            "Found 1 line (Action, \"gun\")"
        );
        assert_eq!(
            done("list_paragraphs", &json!({"scene": "kitchen"}), &one, None),
            "Found 1 line (scene \"kitchen\")"
        );
        assert_eq!(done("list_paragraphs", &json!({}), &json!({"total": 0}).to_string(), None), "Found 0 lines");
        assert_eq!(done("list_paragraphs", &json!({}), "garbled", None), "Searched the lines");
    }

    #[test]
    fn listings() {
        assert_eq!(done("script_info", &Value::Null, "", None), "Looked at the script summary");
        assert_eq!(done("list_reviews", &Value::Null, "[]", None), "Listed reviews");
        assert_eq!(
            done("list_suggestions", &json!({}), "[]", Some("Dialogue pass")),
            "Listed suggestions in Dialogue pass"
        );
        assert_eq!(
            done("list_suggestions", &json!({"review": "Other"}), "[]", Some("Dialogue pass")),
            "Listed suggestions in Other"
        );
        assert_eq!(
            done("list_suggestions", &json!({}), "[]", None),
            "Listed suggestions in this conversation's review"
        );
    }

    #[test]
    fn suggest_change() {
        let r = Some("Dialogue pass");
        assert_eq!(
            done("suggest_change", &json!({"paragraph_id": 812}), "", r),
            "Proposed a change to ¶812 → Dialogue pass"
        );
        assert_eq!(
            done("suggest_change", &json!({"paragraph_ids": [3, 4, 5]}), "", r),
            "Proposed a change to ¶3–¶5 → Dialogue pass"
        );
        assert_eq!(
            done("suggest_change", &json!({"paragraph_ids": [3]}), "", r),
            "Proposed a change to ¶3 → Dialogue pass"
        );
        assert_eq!(
            done("suggest_change", &json!({"insert_after": 9}), "", None),
            "Proposed new lines after ¶9 → this conversation's review"
        );
        assert_eq!(done("suggest_change", &json!({}), "", r), "Proposed a change → Dialogue pass");
    }

    #[test]
    fn suggest_changes() {
        let r = Some("Dialogue pass");
        let ok = json!({"added": 8, "suggestions": [], "failed": []}).to_string();
        assert_eq!(done("suggest_changes", &Value::Null, &ok, r), "Proposed 8 changes → Dialogue pass");
        let some = json!({"added": 8, "suggestions": [], "failed": [{}, {}]}).to_string();
        assert_eq!(done("suggest_changes", &Value::Null, &some, r), "Proposed 8 changes → Dialogue pass · 2 refused");
        let one = json!({"added": 1, "failed": []}).to_string();
        assert_eq!(done("suggest_changes", &Value::Null, &one, r), "Proposed 1 change → Dialogue pass");
        // Nothing passed validation: the result is a sentence.
        let args = json!({"suggestions": [{}, {}, {}]});
        assert_eq!(
            done("suggest_changes", &args, "none of the suggestions were valid", r),
            "Proposed 0 changes · 3 refused"
        );
    }

    #[test]
    fn unknown_and_failed() {
        assert_eq!(done("nope", &Value::Null, "", None), "Ran nope");
        assert_eq!(failed("read_scene", "no scene 99 (1 to 40)"), "Couldn't read the scene");
        assert_eq!(NOT_RUN, "Not run (stopped)");
        assert_eq!(OWN_REVIEW, "this conversation's review");
    }

    #[test]
    fn numbers() {
        assert_eq!(bytes_text(999), "999 B");
        assert_eq!(bytes_text(3200), "3.2 kB");
        assert_eq!(tokens_short(850), "850");
        assert_eq!(tokens_short(18_200), "18.2k");
        assert_eq!(tokens_short(1_300_000), "1.3M");
        assert_eq!(tokens_short(999), "999");
        assert_eq!(tokens_short(1000), "1.0k");
        assert_eq!(tokens_short(999_949), "999.9k");
        assert_eq!(tokens_short(999_950), "1.0M");
        assert_eq!(tokens_short(1_049_999), "1.0M");
        assert_eq!(tokens_short(1_050_000), "1.1M");
        assert_eq!(tokens_short(u64::MAX), "18446744073709.5M");
        assert_eq!(cost_text(0.24), "$0.24");
        assert_eq!(cost_text(0.001), "<$0.01");
        assert_eq!(cost_text(0.0), "$0.00");
    }

    #[test]
    fn usage_lines() {
        assert_eq!(usage_text(&usage(18_000, 200, true, Some(0.24)), None), "18.2k tokens · $0.24");
        // Reported cost wins over the price table.
        assert_eq!(usage_text(&usage(1000, 0, true, Some(0.10)), Some(0.50)), "1.0k tokens · $0.10");
        assert_eq!(usage_text(&usage(1000, 0, true, None), Some(0.5)), "1.0k tokens · $0.50");
        assert_eq!(usage_text(&usage(1000, 0, true, None), None), "1.0k tokens · cost unknown");
        // A stopped reply: the rest is a floor, never wiped.
        assert_eq!(usage_text(&usage(1000, 0, false, None), Some(0.5)), "at least 1.0k tokens · $0.50+");
        assert_eq!(usage_text(&usage(1000, 0, false, None), None), "at least 1.0k tokens · cost unknown");
        assert_eq!(usage_text(&usage(1000, 0, false, None), Some(0.0)), "at least 1.0k tokens · cost unknown");
        assert_eq!(usage_text(&usage(0, 0, false, None), Some(0.5)), "tokens unknown · cost unknown");
        assert_eq!(usage_text(&usage(0, 0, true, None), Some(0.0)), "0 tokens · $0.00");
    }
}
