use anyhow::{Result, bail};
use serde_json::{Value, json};

// Raw HTTP against the Messages API — no official Anthropic Rust SDK exists,
// so this is the documented fallback for unsupported languages. See
// ARCHITECTURE.md for why this is gated on ANTHROPIC_API_KEY rather than
// always-on.
pub const SONNET: &str = "claude-sonnet-5";
pub const OPUS: &str = "claude-opus-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";

pub struct AnthropicClient {
    api_key: String,
    http: reqwest::Client,
    model: String,
    effort: String,
}

impl AnthropicClient {
    // `model`/`effort` are picked per call site: Sonnet 5 at "low" effort
    // for the frequent per-message pass, Opus 5 at "high" for the
    // infrequent bulk initiative assessment.
    pub fn from_env(model: &str, effort: &str) -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        if api_key.trim().is_empty() {
            return None;
        }
        Some(Self {
            api_key,
            http: reqwest::Client::new(),
            model: model.to_string(),
            effort: effort.to_string(),
        })
    }

    pub fn is_configured() -> bool {
        std::env::var("ANTHROPIC_API_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }

    // Sends one user turn with the given tools and returns every tool_use
    // block the model produced.
    pub async fn call_tools(
        &self,
        system: &str,
        user_content: &str,
        tools: Value,
        max_tokens: u32,
    ) -> Result<Vec<(String, Value)>> {
        let body = json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "system": system,
            "tools": tools,
            "tool_choice": {"type": "auto"},
            "output_config": {"effort": self.effort},
            "messages": [{"role": "user", "content": user_content}],
        });

        let resp = self
            .http
            .post(API_URL)
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let payload: Value = resp.json().await?;
        if !status.is_success() {
            bail!("Anthropic API error ({status}): {payload}");
        }

        let mut calls = Vec::new();
        if let Some(content) = payload.get("content").and_then(|c| c.as_array()) {
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    let name = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or(json!({}));
                    calls.push((name, input));
                }
            }
        }
        Ok(calls)
    }
}
