//! Ollama model provider.
//!
//! Communicates with a local Ollama instance via its HTTP API.
//! <https://github.com/ollama/ollama/blob/main/docs/api.md>

use async_trait::async_trait;
use ngenorca_core::{Error, Result};
use ngenorca_plugin_sdk::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ModelInfo, ModelProvider,
    ToolCallResponse, ToolDefinition, Usage,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Ollama provider using the `/api/chat` endpoint.
pub struct OllamaProvider {
    client: Client,
    base_url: String,
    keep_alive: Option<String>,
    num_ctx: Option<usize>,
}

impl OllamaProvider {
    pub fn new(base_url: &str, keep_alive: Option<String>, num_ctx: Option<usize>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            keep_alive,
            num_ctx,
        }
    }
}

// ─── Ollama API types ───────────────────────────────────────────

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OllamaTool>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_ctx: Option<usize>,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OllamaFunction,
}

#[derive(Debug, Serialize)]
struct OllamaFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaToolCall {
    function: OllamaToolCallFunction,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaToolCallFunction {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
    #[serde(default)]
    eval_count: usize,
    #[serde(default)]
    prompt_eval_count: usize,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelInfo>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OllamaModelInfo {
    name: String,
    #[serde(default)]
    details: OllamaModelDetails,
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct OllamaModelDetails {
    #[serde(default)]
    parameter_size: Option<String>,
    #[serde(default)]
    family: Option<String>,
}

// ─── Conversions ────────────────────────────────────────────────

fn to_ollama_messages(messages: &[ChatMessage]) -> Vec<OllamaMessage> {
    messages
        .iter()
        .map(|m| OllamaMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_calls: None,
        })
        .collect()
}

fn to_ollama_tools(tools: &[ToolDefinition]) -> Vec<OllamaTool> {
    tools
        .iter()
        .map(|t| OllamaTool {
            tool_type: "function".into(),
            function: OllamaFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect()
}

// ─── ModelProvider impl ─────────────────────────────────────────

#[async_trait]
impl ModelProvider for OllamaProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Gateway(format!("Ollama list_models: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Gateway(format!(
                "Ollama list_models HTTP {status}: {body}"
            )));
        }

        let tags: OllamaTagsResponse = resp
            .json()
            .await
            .map_err(|e| Error::Gateway(format!("Ollama parse tags: {e}")))?;

        Ok(tags
            .models
            .into_iter()
            .map(|m| ModelInfo {
                id: m.name.clone(),
                name: m.name,
                context_window: self.num_ctx.unwrap_or(4096),
                supports_tools: true, // Ollama supports tools in newer versions
                supports_vision: false,
                is_local: true,
            })
            .collect())
    }

    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        // Strip provider prefix if present (e.g., "ollama/llama3" → "llama3")
        let model = request
            .model
            .strip_prefix("ollama/")
            .unwrap_or(&request.model)
            .to_string();

        let ollama_req = OllamaChatRequest {
            model,
            messages: to_ollama_messages(&request.messages),
            stream: false,
            options: Some(OllamaOptions {
                temperature: request.temperature,
                num_predict: request.max_tokens,
                num_ctx: self.num_ctx,
            }),
            keep_alive: self.keep_alive.clone(),
            tools: request.tools.as_ref().map(|t| to_ollama_tools(t)),
        };

        let url = format!("{}/api/chat", self.base_url);
        debug!(url = %url, "Ollama chat_completion request");

        let resp = self
            .client
            .post(&url)
            .json(&ollama_req)
            .send()
            .await
            .map_err(|e| Error::Gateway(format!("Ollama chat: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Gateway(format!(
                "Ollama chat HTTP {status}: {body}"
            )));
        }

        let ollama_resp: OllamaChatResponse = resp
            .json()
            .await
            .map_err(|e| Error::Gateway(format!("Ollama parse response: {e}")))?;

        // Convert tool calls if present
        let tool_calls = ollama_resp
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, tc)| ToolCallResponse {
                id: format!("call_{i}"),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect();

        let content = if ollama_resp.message.content.is_empty() {
            None
        } else {
            Some(ollama_resp.message.content)
        };

        Ok(ChatCompletionResponse {
            content,
            tool_calls,
            usage: Usage {
                prompt_tokens: ollama_resp.prompt_eval_count,
                completion_tokens: ollama_resp.eval_count,
                total_tokens: ollama_resp.prompt_eval_count + ollama_resp.eval_count,
            },
        })
    }

    fn provider_name(&self) -> &str {
        "ollama"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let p = OllamaProvider::new("http://localhost:11434", None, None);
        assert_eq!(p.provider_name(), "ollama");
    }

    #[test]
    fn to_ollama_messages_converts_correctly() {
        let msgs = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hello".into(),
            },
        ];
        let converted = to_ollama_messages(&msgs);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].role, "system");
        assert_eq!(converted[1].content, "Hello");
    }

    #[test]
    fn to_ollama_tools_converts_correctly() {
        let tools = vec![ToolDefinition {
            name: "search".into(),
            description: "Search the web".into(),
            parameters: serde_json::json!({"type": "object"}),
            requires_sandbox: false,
        }];
        let converted = to_ollama_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].function.name, "search");
        assert_eq!(converted[0].tool_type, "function");
    }

    #[test]
    fn model_prefix_stripping() {
        let model = "ollama/llama3:8b";
        let stripped = model.strip_prefix("ollama/").unwrap_or(model);
        assert_eq!(stripped, "llama3:8b");
    }
}
