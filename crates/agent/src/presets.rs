//! Known providers: what to fill in when the writer picks one. Any provider
//! not listed works as "Custom" with its base URL and protocol.

use crate::Protocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// Stable id, stored in settings.
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: Protocol,
    pub base_url: &'static str,
    /// Suggested model, if we know a good default; otherwise the writer picks from the account's list.
    pub default_model: Option<&'static str>,
    /// Where the writer gets a key.
    pub keys_url: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "anthropic",
        name: "Anthropic",
        protocol: Protocol::Anthropic,
        base_url: "https://api.anthropic.com/v1",
        default_model: Some("claude-opus-5-5"),
        keys_url: "https://platform.claude.com/settings/keys",
    },
    Preset {
        id: "openai",
        name: "OpenAI",
        protocol: Protocol::OpenaiChat,
        base_url: "https://api.openai.com/v1",
        default_model: None,
        keys_url: "https://platform.openai.com/api-keys",
    },
    Preset {
        id: "gemini",
        name: "Google Gemini",
        protocol: Protocol::OpenaiChat,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: None,
        keys_url: "https://aistudio.google.com/apikey",
    },
    Preset {
        id: "openrouter",
        name: "OpenRouter",
        protocol: Protocol::OpenaiChat,
        base_url: "https://openrouter.ai/api/v1",
        default_model: None,
        keys_url: "https://openrouter.ai/keys",
    },
    Preset {
        id: "fireworks",
        name: "Fireworks",
        protocol: Protocol::OpenaiChat,
        base_url: "https://api.fireworks.ai/inference/v1",
        default_model: None,
        keys_url: "https://app.fireworks.ai/settings/users/api-keys",
    },
];

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}
