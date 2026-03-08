//! Anthropic (Claude) model provider.
//!
//! Communicates with the Anthropic Messages API.
//! <https://docs.anthropic.com/en/api/messages>

use async_trait::async_trait;
use ngenorca_core::{Error, Result};
use ngenorca_plugin_sdk::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ModelInfo, ModelProvider,
    ToolCallResponse, ToolDefinition, Usage,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Anthropic Claude provider.
pub struct AnthropicProvider {
    client: Client,
    base_url: String,
    api_key: String,
    api_version: String,
    default_max_tokens: Option<usize>,
    default_temperature: Option<f64>,
}

impl AnthropicProvider {
    pub fn new(
        base_url: &str,
        api_key: String,
        api_version: String,
        default_max_tokens: Option<usize>,
        default_temperature: Option<f64>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            api_version,
            default_max_tokens,
            default_temperature,
        }
    }
}

// ─── Anthropic API types ────────────────────────────────────────

#[derive(Debug, Serialize)]
struct AnthropicMessagesRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessagesResponse {
    content: Vec<AnthropicContentBlock>,
    usage: AnthropicUsage,
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: usize,
    output_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorResponse {
    error: AnthropicApiError,
}

#[derive(Debug, Deserialize)]
struct AnthropicApiError {
    message: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    error_type: String,
}

// ─── Conversions ────────────────────────────────────────────────

fn to_anthropic_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system_prompt = None;
    let mut anthropic_msgs = Vec::new();

    for msg in messages {
        if msg.role == "system" {
            // Anthropic uses a top-level `system` field, not a message
            system_prompt = Some(msg.content.clone());
        } else {
            anthropic_msgs.push(AnthropicMessage {
                role: msg.role.clone(),
                content: AnthropicContent::Text(msg.content.clone()),
            });
        }
    }

    (system_prompt, anthropic_msgs)
}

fn to_anthropic_tools(tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|t| AnthropicTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        })
        .collect()
}

// ─── ModelProvider impl ─────────────────────────────────────────

#[async_trait]
impl ModelProvider for AnthropicProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        // Anthropic doesn't have a list models endpoint that's easily accessible.
        // Return a static list of known models.
        Ok(vec![
            ModelInfo {
                id: "claude-sonnet-4-20250514".into(),
                name: "Claude Sonnet 4".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: true,
                is_local: false,
            },
            ModelInfo {
                id: "claude-opus-4-20250514".into(),
                name: "Claude Opus 4".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: true,
                is_local: false,
            },
            ModelInfo {
                id: "claude-3-5-haiku-20241022".into(),
                name: "Claude 3.5 Haiku".into(),
                context_window: 200_000,
                supports_tools: true,
                supports_vision: true,
                is_local: false,
            },
        ])
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        // Strip provider prefix
        let model = request
            .model
            .strip_prefix("anthropic/")
            .unwrap_or(&request.model)
            .to_string();

        let (system, messages) = to_anthropic_messages(&request.messages);

        // Ensure messages is not empty — Anthropic requires at least one message
        if messages.is_empty() {
            return Err(Error::Gateway(
                "Anthropic requires at least one non-system message".into(),
            ));
        }

        let max_tokens = request
            .max_tokens
            .or(self.default_max_tokens)
            .unwrap_or(4096);

        let temperature = request.temperature.or(self.default_temperature);

        let anthropic_req = AnthropicMessagesRequest {
            model,
            messages,
            max_tokens,
            system,
            temperature,
            tools: request.tools.as_ref().map(|t| to_anthropic_tools(t)),
        };

        let url = format!("{}/v1/messages", self.base_url);
        debug!(url = %url, "Anthropic chat_completion request");

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", &self.api_version)
            .header("content-type", "application/json")
            .json(&anthropic_req)
            .send()
            .await
            .map_err(|e| super::map_provider_transport_error("Anthropic", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Try to extract structured error message
            let display_body = if let Ok(err) = serde_json::from_str::<AnthropicErrorResponse>(&body) {
                err.error.message
            } else {
                body
            };
            return Err(super::map_provider_http_error("Anthropic", status, display_body));
        }

        let anthropic_resp: AnthropicMessagesResponse = resp
            .json()
            .await
            .map_err(|e| Error::Gateway(format!("Anthropic parse response: {e}")))?;

        // Extract text content and tool calls
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &anthropic_resp.content {
            match block {
                AnthropicContentBlock::Text { text } => {
                    text_parts.push(text.clone());
                }
                AnthropicContentBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCallResponse {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: input.clone(),
                    });
                }
                _ => {}
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let total = anthropic_resp.usage.input_tokens + anthropic_resp.usage.output_tokens;

        Ok(ChatCompletionResponse {
            content,
            tool_calls,
            usage: Usage {
                prompt_tokens: anthropic_resp.usage.input_tokens,
                completion_tokens: anthropic_resp.usage.output_tokens,
                total_tokens: total,
            },
        })
    }

    fn provider_name(&self) -> &str {
        "anthropic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let p = AnthropicProvider::new("https://api.anthropic.com", "sk-test".into(), "2023-06-01".into(), None, None);
        assert_eq!(p.provider_name(), "anthropic");
    }

    #[test]
    fn to_anthropic_messages_extracts_system() {
        let msgs = vec![
            ChatMessage {
                role: "system".into(),
                content: "Be helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hello".into(),
            },
        ];
        let (system, converted) = to_anthropic_messages(&msgs);
        assert_eq!(system, Some("Be helpful.".into()));
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "user");
    }

    #[test]
    fn to_anthropic_tools_converts() {
        let tools = vec![ToolDefinition {
            name: "calc".into(),
            description: "Calculator".into(),
            parameters: serde_json::json!({"type": "object"}),
            requires_sandbox: false,
        }];
        let converted = to_anthropic_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].name, "calc");
    }

    #[test]
    fn model_prefix_stripping() {
        let model = "anthropic/claude-sonnet-4-20250514";
        let stripped = model.strip_prefix("anthropic/").unwrap_or(model);
        assert_eq!(stripped, "claude-sonnet-4-20250514");
    }

    #[test]
    fn static_model_list() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let p = AnthropicProvider::new("https://api.anthropic.com", "sk-test".into(), "2023-06-01".into(), None, None);
        let models = rt.block_on(p.list_models()).unwrap();
        assert!(models.len() >= 3);
        assert!(models.iter().any(|m| m.id.contains("sonnet")));
    }
}
