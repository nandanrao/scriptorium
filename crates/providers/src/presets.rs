//! The providers offered by name: what to fill in when the writer picks one.
//! Any other endpoint works as [`crate::Provider::Custom`] with its base URL
//! and protocol.
//!
//! The model lists are short suggestions for the picker, not limits: an
//! account may name any model its endpoint serves, and the shell can list
//! the endpoint's own models ([`crate::models_request`]). Model ids checked
//! 2026-11-15 for Anthropic; the OpenAI and OpenRouter ids should be checked
//! against their model lists before a release.

use scriptorium_agent::Protocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// Stable id, stored in the accounts file (`"provider": "<id>"`).
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: Protocol,
    pub base_url: &'static str,
    /// The model new accounts start with.
    pub default_model: &'static str,
    /// Suggested models, best first; includes `default_model`.
    pub models: &'static [&'static str],
    /// Where the writer gets a key.
    pub keys_url: &'static str,
}

pub const ANTHROPIC: Preset = Preset {
    id: "anthropic",
    name: "Anthropic",
    protocol: Protocol::Anthropic,
    base_url: "https://api.anthropic.com/v1",
    default_model: "claude-opus-5-5",
    models: &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-2"],
    keys_url: "https://platform.claude.com/settings/keys",
};

/// Current models that call tools over Chat Completions
/// (<https://developers.openai.com/api/docs/models>): GPT-6 Sol, GPT-5.6
/// Terra and GPT-6 Luna only without reasoning (the request sends
/// `reasoning_effort: "none"`, see [`crate::ChatTools`]); GPT-5.2 at any
/// effort. GPT-6 Astra, GPT-6.1 Sol and GPT-6.1 Terra call tools only on the
/// Responses API, so they aren't offered.
pub const OPENAI: Preset = Preset {
    id: "openai",
    name: "OpenAI",
    protocol: Protocol::OpenaiChat,
    base_url: "https://api.openai.com/v1",
    default_model: "gpt-6-sol",
    models: &["gpt-6-sol", "gpt-5.6-terra", "gpt-6-luna", "gpt-5.2"],
    keys_url: "https://platform.openai.com/api-keys",
};

pub const OPENROUTER: Preset = Preset {
    id: "openrouter",
    name: "OpenRouter",
    protocol: Protocol::OpenaiChat,
    base_url: "https://openrouter.ai/api/v1",
    default_model: "anthropic/claude-opus-5.5",
    models: &[
        "anthropic/claude-opus-5.5",
        "anthropic/claude-sonnet-5.5",
        "anthropic/claude-haiku-5.5",
        "anthropic/claude-fable-5.2",
    ],
    keys_url: "https://openrouter.ai/keys",
};

/// Every preset, in the order to list them.
pub const PRESETS: &[Preset] = &[ANTHROPIC, OPENAI, OPENROUTER];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_consistent() {
        for p in PRESETS {
            assert!(p.models.contains(&p.default_model), "{}", p.id);
            assert!(p.base_url.starts_with("https://") && !p.base_url.ends_with('/'), "{}", p.id);
        }
    }
}
