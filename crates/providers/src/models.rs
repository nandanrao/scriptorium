//! What we know about models: what they accept and what they cost.
//!
//! Only Anthropic's models are listed for now (see the table for where the
//! prices come from and when they were checked). OpenAI's and other
//! endpoints' models are unknown here: their requests send only what the
//! writer set, and their cost is the provider's own figure (OpenRouter
//! reports one) or a price the writer adds to the account
//! ([`crate::Account::prices`]). Keep this table current when models change.

use serde::{Deserialize, Serialize};

use scriptorium_agent::Effort;

/// What a model accepts, for leaving out what it would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Input tokens it can take, for "the next message sends about 14k of 1M".
    pub context_window: u64,
    /// Its largest `max_tokens`.
    pub max_output: u64,
    /// Whether it takes `thinking: {type: "adaptive"}` ([`scriptorium_agent::Thinking::Adaptive`]).
    pub adaptive_thinking: bool,
    /// The effort levels it accepts, lowest first; empty if it takes none.
    pub efforts: &'static [Effort],
}

impl Capabilities {
    /// The effort to send for `wanted`: itself if accepted, else the highest
    /// accepted level below it, else the lowest accepted level; `None` if the
    /// model takes no effort at all.
    pub fn effort(&self, wanted: Effort) -> Option<Effort> {
        if self.efforts.contains(&wanted) {
            return Some(wanted);
        }
        let below = self.efforts.iter().copied().filter(|e| rank(*e) < rank(wanted)).max_by_key(|e| rank(*e));
        below.or_else(|| self.efforts.first().copied())
    }
}

fn rank(e: Effort) -> u8 {
    match e {
        Effort::Low => 0,
        Effort::Medium => 1,
        Effort::High => 2,
        Effort::XHigh => 3,
        Effort::Max => 4,
    }
}

/// A model's price. **Every rate is in US dollars per million tokens**
/// (in the accounts file too: `{"input": 3.0, "output": 15.0}` is $3 and
/// $15 per million).
///
/// `input` is the base rate for input tokens not read from or written to the
/// prompt cache. A cache rate of `None` means unknown: a reply that used the
/// cache then has no known cost (rather than a wrong one).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// Input tokens read from the prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// Input tokens written to the prompt cache (Anthropic's 5-minute
    /// cache, which is what the agent crate asks for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    /// Different rates for long prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<PriceTier>,
}

/// Rates for a request whose input (all of it, cache included) is more than
/// `input_tokens` tokens. They replace the base rates for the whole request.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PriceTier {
    pub input_tokens: u64,
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

impl Price {
    /// The rates for a request with `input` tokens of input: `(input,
    /// output, cache_read, cache_write)`.
    pub fn rates(&self, input: u64) -> (f64, f64, Option<f64>, Option<f64>) {
        match self.above {
            Some(t) if input > t.input_tokens => (t.input, t.output, t.cache_read, t.cache_write),
            _ => (self.input, self.output, self.cache_read, self.cache_write),
        }
    }

    /// Every rate is finite and not negative.
    pub fn is_valid(&self) -> bool {
        let ok = |r: f64| r.is_finite() && r >= 0.0;
        let opt = |r: Option<f64>| r.is_none_or(ok);
        ok(self.input)
            && ok(self.output)
            && opt(self.cache_read)
            && opt(self.cache_write)
            && self.above.is_none_or(|t| ok(t.input) && ok(t.output) && opt(t.cache_read) && opt(t.cache_write))
    }
}

const ALL: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::XHigh, Effort::Max];
/// Opus 4.6 and Sonnet 4.6: `xhigh` came later.
const NO_XHIGH: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High, Effort::Max];

const M: u64 = 1_000_000;

const fn current(context_window: u64) -> Capabilities {
    Capabilities { context_window, max_output: 128_000, adaptive_thinking: true, efforts: ALL }
}

/// A price on Anthropic's standard rules: cache reads at 0.1× input, cache
/// writes (5-minute) at 1.25× input.
const fn anthropic(input: f64, output: f64) -> Price {
    Price { input, output, cache_read: Some(input * 0.1), cache_write: Some(input * 1.25), above: None }
}

/// The standard rules, except for a cache-read rate Anthropic publishes
/// for the model.
const fn anthropic_read(input: f64, output: f64, cache_read: f64) -> Price {
    Price { cache_read: Some(cache_read), ..anthropic(input, output) }
}

struct Model {
    id: &'static str,
    caps: Capabilities,
    price: Price,
}

/// Anthropic's first-party rates, checked 2026-11-15: input and output
/// prices, context windows, output limits and Haiku 5.5's long-prompt rate
/// as published; cache rates on the standard rules (see [`anthropic`]) except
/// where a model's own cache-read rate is published (Fable/Mythos 5.2 and
/// 5.1, Opus 5.5, Sonnet 5.5).
const ANTHROPIC: &[Model] = &[
    Model { id: "claude-fable-5-2", caps: current(M), price: anthropic_read(8.0, 40.0, 0.20) },
    Model { id: "claude-mythos-5-2", caps: current(M), price: anthropic_read(8.0, 40.0, 0.20) },
    Model { id: "claude-fable-5-1", caps: current(M), price: anthropic_read(10.0, 50.0, 0.25) },
    Model { id: "claude-mythos-5-1", caps: current(M), price: anthropic_read(10.0, 50.0, 0.25) },
    Model { id: "claude-fable-5", caps: current(M), price: anthropic(10.0, 50.0) },
    Model { id: "claude-opus-5-5", caps: current(M), price: anthropic_read(4.0, 20.0, 0.20) },
    Model { id: "claude-opus-5", caps: current(M), price: anthropic(5.0, 25.0) },
    Model { id: "claude-opus-4-8", caps: current(M), price: anthropic(5.0, 25.0) },
    Model { id: "claude-opus-4-7", caps: current(M), price: anthropic(5.0, 25.0) },
    Model {
        id: "claude-opus-4-6",
        caps: Capabilities { context_window: M, max_output: 128_000, adaptive_thinking: true, efforts: NO_XHIGH },
        price: anthropic(5.0, 25.0),
    },
    Model { id: "claude-sonnet-5-5", caps: current(M), price: anthropic_read(2.0, 10.0, 0.10) },
    Model { id: "claude-sonnet-5", caps: current(M), price: anthropic(2.0, 10.0) },
    Model {
        id: "claude-sonnet-4-6",
        caps: Capabilities { context_window: M, max_output: 128_000, adaptive_thinking: true, efforts: NO_XHIGH },
        price: anthropic(3.0, 15.0),
    },
    Model {
        id: "claude-haiku-5-5",
        caps: current(M),
        // Prompts over 100k tokens: $0.50 / $2.50, cache rates on the standard rules.
        price: Price {
            above: Some(PriceTier {
                input_tokens: 100_000,
                input: 0.50,
                output: 2.50,
                cache_read: Some(0.05),
                cache_write: Some(0.625),
            }),
            ..anthropic(0.10, 0.50)
        },
    },
    Model {
        id: "claude-haiku-4-5",
        caps: Capabilities { context_window: 200_000, max_output: 64_000, adaptive_thinking: false, efforts: &[] },
        price: anthropic(1.0, 5.0),
    },
];

/// The table entry for `model`, also found by the name other endpoints
/// give it: OpenRouter's `anthropic/claude-opus-4.6` or
/// `anthropic/claude-sonnet-4.6:thinking` (vendor prefix and `:variant`
/// dropped, dots read as dashes).
fn find(model: &str) -> Option<&'static Model> {
    let bare = model.rsplit('/').next().unwrap_or(model);
    let bare = bare.split(':').next().unwrap_or(bare).replace('.', "-");
    ANTHROPIC.iter().find(|m| m.id == model || m.id == bare)
}

/// What `model` accepts, if it's a model we know.
pub fn capabilities(model: &str) -> Option<Capabilities> {
    find(model).map(|m| m.caps)
}

/// Anthropic's first-party price for `model` (exact id: other endpoints
/// selling the same model charge their own prices).
pub fn anthropic_price(model: &str) -> Option<Price> {
    ANTHROPIC.iter().find(|m| m.id == model).map(|m| m.price)
}

/// Every model in the table with its price, for listing.
pub fn anthropic_models() -> impl Iterator<Item = (&'static str, Capabilities, Price)> {
    ANTHROPIC.iter().map(|m| (m.id, m.caps, m.price))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        let opus = capabilities("claude-opus-5-5").unwrap();
        assert!(opus.adaptive_thinking);
        assert_eq!(opus.max_output, 128_000);
        assert_eq!(capabilities("anthropic/claude-opus-5-5"), Some(opus));
        // OpenRouter's ids.
        assert_eq!(capabilities("anthropic/claude-opus-5.5"), Some(opus));
        assert_eq!(capabilities("anthropic/claude-opus-4.6").unwrap().efforts, NO_XHIGH);
        assert_eq!(capabilities("anthropic/claude-sonnet-4.6:thinking").unwrap().efforts, NO_XHIGH);
        assert_eq!(capabilities("anthropic/claude-haiku-4.5").unwrap().max_output, 64_000);
        assert_eq!(capabilities("openai/gpt-5"), None);
        assert_eq!(capabilities("claude-haiku-4-5").unwrap().context_window, 200_000);
        assert_eq!(capabilities("gpt-5"), None);
        let opus_price = anthropic_price("claude-opus-5-5").unwrap();
        assert_eq!((opus_price.cache_read, opus_price.cache_write), (Some(0.20), Some(5.0)));
        let sonnet46 = anthropic_price("claude-sonnet-4-6").unwrap();
        assert!((sonnet46.cache_read.unwrap() - 0.30).abs() < 1e-12, "0.1× input");
        assert!((sonnet46.cache_write.unwrap() - 3.75).abs() < 1e-12, "1.25× input");
        assert_eq!(anthropic_price("anthropic/claude-opus-5-5"), None, "only the first-party id");
    }

    #[test]
    fn effort_is_lowered_to_what_the_model_takes() {
        let sonnet46 = capabilities("claude-sonnet-4-6").unwrap();
        assert_eq!(sonnet46.effort(Effort::XHigh), Some(Effort::High));
        assert_eq!(sonnet46.effort(Effort::Max), Some(Effort::Max));
        assert_eq!(capabilities("claude-haiku-4-5").unwrap().effort(Effort::Low), None);
        let odd = Capabilities { context_window: 1, max_output: 1, adaptive_thinking: false, efforts: &[Effort::High] };
        assert_eq!(odd.effort(Effort::Low), Some(Effort::High), "nothing below: the lowest accepted");
    }

    #[test]
    fn tiers_and_validity() {
        let haiku = anthropic_price("claude-haiku-5-5").unwrap();
        assert_eq!(haiku.rates(100_000).0, 0.10);
        assert_eq!(haiku.rates(100_001).0, 0.50);
        assert!(anthropic_models().all(|(_, _, p)| p.is_valid()));
        assert!(!Price { input: f64::NAN, output: 0.0, cache_read: None, cache_write: None, above: None }.is_valid());
        let ids: Vec<_> = anthropic_models().map(|(id, ..)| id).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(ids.len(), unique.len());
    }
}
