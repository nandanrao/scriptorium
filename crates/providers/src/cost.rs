//! What a thread has cost, for the token/cost display.
//!
//! A reply's cost is the provider's own figure when it reports one
//! (OpenRouter's `usage.cost`), else its token counts times the price for
//! the endpoint and model that wrote it. Anything not known is reported as
//! not known, never guessed: a total is complete only if every reply's cost is.

use scriptorium_agent::{Origin, Thread, Usage};

use crate::account::{AccountsFile, Provider};
use crate::models::{Price, anthropic_models};

/// Prices by endpoint and model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PriceBook {
    /// Later entries win.
    entries: Vec<(Origin, Price)>,
}

impl PriceBook {
    /// Anthropic's first-party prices, at Anthropic's endpoint only (a proxy
    /// or reseller of the same model charges its own).
    pub fn builtin() -> Self {
        let base = Provider::Anthropic.base_url();
        PriceBook { entries: anthropic_models().map(|(id, _, price)| (Origin::new(base, id), price)).collect() }
    }

    /// The built-in prices plus every account's own ([`crate::Account::prices`]),
    /// which win for the account's endpoint.
    pub fn from_accounts(file: &AccountsFile) -> Self {
        let mut book = PriceBook::builtin();
        for a in &file.accounts {
            for (model, price) in &a.prices {
                book.insert(Origin::new(a.provider.base_url(), model), *price);
            }
        }
        book
    }

    pub fn insert(&mut self, origin: Origin, price: Price) {
        self.entries.push((origin, price));
    }

    pub fn price(&self, origin: &Origin) -> Option<Price> {
        self.entries.iter().rev().find(|(o, _)| o == origin).map(|(_, p)| *p)
    }
}

impl Price {
    /// What `tokens` of fresh (uncached) input cost, in US dollars: for
    /// "reading the whole script costs about $0.18" before it's sent.
    pub fn input_cost(&self, tokens: u64) -> f64 {
        self.rates(tokens).0 * tokens as f64 / 1e6
    }
}

/// One reply's cost in US dollars: the provider's figure, else `usage`
/// priced with `price`. `None` when the counts are incomplete, there's no
/// price, or the reply used the cache and the price has no cache rate.
pub fn reply_cost(price: Option<&Price>, usage: &Usage) -> Option<f64> {
    if let Some(cost) = usage.cost {
        return Some(cost);
    }
    if !usage.known {
        return None;
    }
    let (input, output, cache_read, cache_write) = price?.rates(usage.input);
    let part = |tokens: u64, rate: Option<f64>| if tokens == 0 { Some(0.0) } else { rate.map(|r| r * tokens as f64) };
    let total = usage.uncached_input() as f64 * input
        + usage.output as f64 * output
        + part(usage.cached, cache_read)?
        + part(usage.cache_write, cache_write)?;
    Some(total / 1e6)
}

/// A thread's cost so far.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreadCost {
    /// The sum of the replies whose cost is known, in US dollars.
    pub usd: f64,
    /// Whether every reply's cost is known (else `usd` is a lower bound).
    pub complete: bool,
}

/// Every reply's cost added up, each priced for the endpoint and model that
/// wrote it (a thread may have run on several), withdrawn replies included
/// (as [`Thread::usage`] counts them).
pub fn thread_cost(book: &PriceBook, thread: &Thread) -> ThreadCost {
    let mut total = ThreadCost { usd: 0.0, complete: true };
    for m in &thread.messages {
        let Some(usage) = &m.usage else { continue };
        let price = m.origin.as_ref().and_then(|o| book.price(o));
        match reply_cost(price.as_ref(), usage) {
            Some(c) => total.usd += c,
            None => total.complete = false,
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Account;
    use scriptorium_agent::{Block, Message, Protocol};

    const ANTHROPIC: &str = "https://api.anthropic.com/v1";

    fn usage(input: u64, output: u64, cached: u64, cache_write: u64) -> Usage {
        Usage { input, output, cached, cache_write, ..Usage::default() }
    }

    fn reply(endpoint: &str, model: &str, u: Usage) -> Message {
        let mut m = Message::assistant(vec![Block::Text { text: "ok".into() }]);
        m.origin = Some(Origin::new(endpoint, model));
        m.usage = Some(u);
        m
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn prices_each_kind_of_token() {
        let book = PriceBook::builtin();
        let opus = book.price(&Origin::new("https://api.anthropic.com/v1/", "claude-opus-5-5")).unwrap();
        // 1M input of which 200k cached and 100k written; 50k output.
        let c = reply_cost(Some(&opus), &usage(1_000_000, 50_000, 200_000, 100_000)).unwrap();
        // 700k × $4 + 200k × $0.20 + 100k × $5 + 50k × $20, per million.
        assert!(close(c, 2.8 + 0.04 + 0.5 + 1.0), "{c}");
        assert!(close(opus.input_cost(35_000), 0.14));
    }

    #[test]
    fn long_prompt_tier() {
        let haiku = PriceBook::builtin().price(&Origin::new(ANTHROPIC, "claude-haiku-5-5")).unwrap();
        assert!(close(reply_cost(Some(&haiku), &usage(100_000, 0, 0, 0)).unwrap(), 0.01));
        assert!(close(reply_cost(Some(&haiku), &usage(200_000, 0, 0, 0)).unwrap(), 0.10));
    }

    #[test]
    fn unknown_is_not_guessed() {
        let opus = PriceBook::builtin().price(&Origin::new(ANTHROPIC, "claude-opus-5-5")).unwrap();
        assert_eq!(reply_cost(Some(&opus), &Usage::unknown()), None);
        assert_eq!(reply_cost(None, &usage(10, 10, 0, 0)), None);
        let no_cache = Price { input: 1.0, output: 2.0, cache_read: None, cache_write: None, above: None };
        assert_eq!(reply_cost(Some(&no_cache), &usage(10, 10, 5, 0)), None);
        assert_eq!(reply_cost(Some(&no_cache), &usage(10, 10, 0, 5)), None);
        assert!(close(reply_cost(Some(&no_cache), &usage(1_000_000, 0, 0, 0)).unwrap(), 1.0));
        // The provider's own figure wins, even without a price.
        assert_eq!(reply_cost(None, &Usage { cost: Some(0.25), ..usage(10, 10, 0, 0) }), Some(0.25));
    }

    #[test]
    fn builtin_prices_only_at_anthropic() {
        let book = PriceBook::builtin();
        assert!(book.price(&Origin::new("https://openrouter.ai/api/v1", "claude-opus-5-5")).is_none());
        assert!(book.price(&Origin::new("https://proxy.example/v1", "claude-opus-5-5")).is_none());
    }

    #[test]
    fn thread_over_several_models_and_account_prices() {
        let mut file = AccountsFile::default();
        let mut local = Account::new(
            "lm",
            "LM Studio",
            Provider::Custom { protocol: Protocol::OpenaiChat, base_url: "http://localhost:1234/v1".into() },
        );
        local.prices.insert(
            "qwen3".into(),
            Price { input: 0.0, output: 0.0, cache_read: None, cache_write: None, above: None },
        );
        file.upsert(local).unwrap();
        let mut anth = Account::new("a", "A", Provider::Anthropic);
        // An account's price wins over the built-in one at its endpoint.
        anth.prices.insert(
            "claude-sonnet-5-5".into(),
            Price { input: 1.0, output: 1.0, cache_read: Some(0.0), cache_write: Some(0.0), above: None },
        );
        file.upsert(anth).unwrap();
        let book = PriceBook::from_accounts(&file);

        let mut t = Thread::default();
        t.messages.push(Message::user("hi"));
        t.messages.push(reply(ANTHROPIC, "claude-opus-5-5", usage(1_000_000, 0, 0, 0)));
        t.messages.push(reply(ANTHROPIC, "claude-sonnet-5-5", usage(1_000_000, 0, 0, 0)));
        t.messages.push(reply("http://localhost:1234/v1", "qwen3", usage(5_000, 100, 0, 0)));
        assert_eq!(thread_cost(&book, &t), ThreadCost { usd: 5.0, complete: true });

        t.messages.push(reply("https://api.openai.com/v1", "gpt-9", usage(10, 10, 0, 0)));
        let c = thread_cost(&book, &t);
        assert!(!c.complete);
        assert!(close(c.usd, 5.0));
        assert_eq!(thread_cost(&book, &Thread::default()), ThreadCost { usd: 0.0, complete: true });
    }
}
