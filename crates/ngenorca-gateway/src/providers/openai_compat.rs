//! OpenAI-compatible model provider.
//!
//! Works with OpenAI, Azure OpenAI, OpenRouter, vLLM, LM Studio,
//! LocalAI, and any other OpenAI-compatible API.

use async_trait::async_trait;
use ngenorca_core::{Error, Result};
use ngenorca_plugin_sdk::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ModelInfo, ModelProvider,
    ToolCallResponse, ToolDefinition, Usage,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// OpenAI-compatible provider (works with OpenAI, Azure, OpenRouter, vLLM, etc.).
pub struct OpenAICompatProvider {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    organization: Option<String>,
    provider_label: String,
    default_max_tokens: Option<usize>,
    default_temperature: Option<f64>,
}

impl OpenAICompatProvider {
    /// Create a new OpenAI-compatible provider.
    pub fn new(
        base_url: &str,
        api_key: Option<String>,
        organization: Option<String>,
        provider_label: &str,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            organization,
            provider_label: provider_label.to_string(),
            default_max_tokens: None,
            default_temperature: None,
        }
    }

    /// Create with default token/temp settings.
    pub fn with_defaults(
        base_url: &str,
        api_key: Option<String>,
        organization: Option<String>,
        provider_label: &str,
        max_tokens: Option<usize>,
        temperature: Option<f64>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            organization,
            provider_label: provider_label.to_string(),
            default_max_tokens: max_tokens,
            default_temperature: temperature,
        }
    }
}

// ─── OpenAI API types ───────────────────────────────────────────

#[derive(Debug, Serialize)]
struct OpenAIChatRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIMessage {
    role: String,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpenAITool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Serialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIToolCallFunction {
    name: String,
    arguments: String, // JSON string for OpenAI
}

#[derive(Debug, Deserialize)]
struct OpenAIChatResponse {
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct OpenAIModelsResponse {
    data: Vec<OpenAIModelEntry>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModelEntry {
    id: String,
    #[allow(dead_code)]
    owned_by: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIErrorResponse {
    error: OpenAIApiError,
}

#[derive(Debug, Deserialize)]
struct OpenAIApiError {
    message: String,
}

// ─── Conversions ────────────────────────────────────────────────

fn to_openai_messages(messages: &[ChatMessage]) -> Vec<OpenAIMessage> {
    messages
        .iter()
        .map(|m| OpenAIMessage {
            role: m.role.clone(),
            content: Some(m.content.clone()),
            tool_calls: None,
            tool_call_id: None,
        })
        .collect()
}

fn to_openai_tools(tools: &[ToolDefinition]) -> Vec<OpenAITool> {
    tools
        .iter()
        .map(|t| OpenAITool {
            tool_type: "function".into(),
            function: OpenAIFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect()
}

// ─── ModelProvider impl ─────────────────────────────────────────

#[async_trait]
impl ModelProvider for OpenAICompatProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/models", self.base_url);

        let mut req = self.client.get(&url);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        if let Some(org) = &self.organization {
            req = req.header("OpenAI-Organization", org);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Gateway(format!("{} list_models: {e}", self.provider_label)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Gateway(format!(
                "{} list_models HTTP {status}: {body}",
                self.provider_label
            )));
        }

        let models_resp: OpenAIModelsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Gateway(format!("{} parse models: {e}", self.provider_label)))?;

        Ok(models_resp
            .data
            .into_iter()
            .map(|m| ModelInfo {
                name: m.id.clone(),
                id: m.id,
                context_window: 128_000, // Varies by model; reasonable default
                supports_tools: true,
                supports_vision: false,
                is_local: false,
            })
            .collect())
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        // Strip common provider prefixes
        let model = request
            .model
            .strip_prefix("openai/")
            .or_else(|| request.model.strip_prefix("openrouter/"))
            .or_else(|| request.model.strip_prefix("azure/"))
            .or_else(|| request.model.strip_prefix("custom/"))
            .unwrap_or(&request.model)
            .to_string();

        let openai_req = OpenAIChatRequest {
            model,
            messages: to_openai_messages(&request.messages),
            max_tokens: request.max_tokens.or(self.default_max_tokens),
            temperature: request.temperature.or(self.default_temperature),
            tools: request.tools.as_ref().map(|t| to_openai_tools(t)),
        };

        let url = format!("{}/chat/completions", self.base_url);
        debug!(url = %url, provider = %self.provider_label, "OpenAI-compat chat_completion");

        let mut req = self.client.post(&url);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        if let Some(org) = &self.organization {
            req = req.header("OpenAI-Organization", org);
        }

        let resp = req
            .json(&openai_req)
            .send()
            .await
            .map_err(|e| Error::Gateway(format!("{} chat: {e}", self.provider_label)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<OpenAIErrorResponse>(&body) {
                return Err(Error::Gateway(format!(
                    "{} HTTP {status}: {}",
                    self.provider_label, err.error.message
                )));
            }
            return Err(Error::Gateway(format!(
                "{} HTTP {status}: {body}",
                self.provider_label
            )));
        }

        let openai_resp: OpenAIChatResponse = resp
            .json()
            .await
            .map_err(|e| Error::Gateway(format!("{} parse response: {e}", self.provider_label)))?;

        let choice = openai_resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::Gateway("No choices in response".into()))?;

        // Convert tool calls
        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let arguments: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or_default();
                ToolCallResponse {
                    id: tc.id,
                    name: tc.function.name,
                    arguments,
                }
            })
            .collect();

        let usage = openai_resp.usage.unwrap_or(OpenAIUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        });

        Ok(ChatCompletionResponse {
            content: choice.message.content,
            tool_calls,
            usage: Usage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
            },
        })
    }

    fn provider_name(&self) -> &str {
        &self.provider_label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_label() {
        let p = OpenAICompatProvider::new(
            "https://api.openai.com/v1",
            Some("sk-test".into()),
            None,
            "openai",
        );
        assert_eq!(p.provider_name(), "openai");
    }

    #[test]
    fn to_openai_messages_converts() {
        let msgs = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hi".into(),
            },
        ];
        let converted = to_openai_messages(&msgs);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].role, "system");
        assert_eq!(converted[0].content, Some("You are helpful.".into()));
    }

    #[test]
    fn to_openai_tools_converts() {
        let tools = vec![ToolDefinition {
            name: "search".into(),
            description: "Search".into(),
            parameters: serde_json::json!({}),
            requires_sandbox: false,
        }];
        let converted = to_openai_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].tool_type, "function");
        assert_eq!(converted[0].function.name, "search");
    }

    #[test]
    fn model_prefix_stripping() {
        for (input, expected) in [
            ("openai/gpt-4o", "gpt-4o"),
            ("openrouter/meta-llama/llama-3", "meta-llama/llama-3"),
            ("azure/gpt-4", "gpt-4"),
            ("custom/my-model", "my-model"),
            ("gpt-4o", "gpt-4o"),
        ] {
            let stripped = input
                .strip_prefix("openai/")
                .or_else(|| input.strip_prefix("openrouter/"))
                .or_else(|| input.strip_prefix("azure/"))
                .or_else(|| input.strip_prefix("custom/"))
                .unwrap_or(input);
            assert_eq!(stripped, expected, "Failed for input: {input}");
        }
    }
}
