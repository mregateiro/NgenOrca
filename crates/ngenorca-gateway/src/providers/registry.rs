//! Provider registry — resolves model names to the correct [`ModelProvider`].
//!
//! When the orchestrator routes a task to `"anthropic/claude-sonnet-4"` or
//! `"ollama/llama3:8b"`, the registry finds the correct provider to call.

use ngenorca_config::NgenOrcaConfig;
use ngenorca_core::{Error, Result};
use ngenorca_plugin_sdk::{
    ChatCompletionRequest, ChatCompletionResponse, ModelInfo, ModelProvider,
};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

use super::anthropic::AnthropicProvider;
use super::ollama::OllamaProvider;
use super::openai_compat::OpenAICompatProvider;

/// Central registry that maps provider names to their implementations.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn ModelProvider>>,
    /// The default provider name (parsed from config `agent.model`).
    default_provider: String,
}

impl ProviderRegistry {
    /// Build the registry from the application config.
    /// Each configured provider gets instantiated.
    pub fn from_config(config: &NgenOrcaConfig) -> Self {
        let mut providers = HashMap::<String, Arc<dyn ModelProvider>>::new();

        // ── Ollama ──
        if let Some(ref ollama_cfg) = config.agent.providers.ollama {
            let provider = Arc::new(OllamaProvider::new(
                &ollama_cfg.base_url,
                ollama_cfg.keep_alive.clone(),
                ollama_cfg.num_ctx,
            ));
            providers.insert("ollama".into(), provider);
            info!("Registered provider: ollama ({})", ollama_cfg.base_url);
        }

        // ── Anthropic ──
        if let Some(ref anthropic_cfg) = config.agent.providers.anthropic {
            if let Some(ref api_key) = anthropic_cfg.api_key {
                let provider = Arc::new(AnthropicProvider::new(
                    &anthropic_cfg.base_url,
                    api_key.clone(),
                    anthropic_cfg.max_tokens,
                    anthropic_cfg.temperature,
                ));
                providers.insert("anthropic".into(), provider);
                info!(
                    "Registered provider: anthropic ({})",
                    anthropic_cfg.base_url
                );
            } else {
                warn!("Anthropic configured but missing API key — skipping");
            }
        }

        // ── OpenAI ──
        if let Some(ref openai_cfg) = config.agent.providers.openai {
            let provider = Arc::new(OpenAICompatProvider::with_defaults(
                &openai_cfg.base_url,
                openai_cfg.api_key.clone(),
                openai_cfg.organization.clone(),
                "openai",
                openai_cfg.max_tokens,
                openai_cfg.temperature,
            ));
            providers.insert("openai".into(), provider);
            info!("Registered provider: openai ({})", openai_cfg.base_url);
        }

        // ── Azure OpenAI ──
        if let Some(ref azure_cfg) = config.agent.providers.azure {
            if let (Some(endpoint), Some(api_key)) =
                (&azure_cfg.endpoint, &azure_cfg.api_key)
            {
                // Azure uses a different URL pattern:
                // {endpoint}/openai/deployments/{deployment}/chat/completions?api-version={version}
                // We'll use the OpenAI-compat provider with the deployment URL
                let base = if let Some(ref deployment) = azure_cfg.deployment {
                    format!(
                        "{}/openai/deployments/{}",
                        endpoint.trim_end_matches('/'),
                        deployment
                    )
                } else {
                    format!("{}/openai", endpoint.trim_end_matches('/'))
                };

                let provider = Arc::new(OpenAICompatProvider::new(
                    &base,
                    Some(api_key.clone()),
                    None,
                    "azure",
                ));
                providers.insert("azure".into(), provider);
                info!("Registered provider: azure");
            }
        }

        // ── Google Gemini ──
        if let Some(ref google_cfg) = config.agent.providers.google {
            if let Some(ref api_key) = google_cfg.api_key {
                // Google has its own API, but can work with OpenAI-compat via
                // generativelanguage.googleapis.com/v1beta/openai
                let base = format!(
                    "{}/openai",
                    google_cfg.base_url.trim_end_matches('/')
                );
                let provider = Arc::new(OpenAICompatProvider::new(
                    &base,
                    Some(api_key.clone()),
                    None,
                    "google",
                ));
                providers.insert("google".into(), provider);
                info!("Registered provider: google");
            }
        }

        // ── OpenRouter ──
        if let Some(ref or_cfg) = config.agent.providers.openrouter {
            if let Some(ref api_key) = or_cfg.api_key {
                let provider = Arc::new(OpenAICompatProvider::new(
                    &or_cfg.base_url,
                    Some(api_key.clone()),
                    None,
                    "openrouter",
                ));
                providers.insert("openrouter".into(), provider);
                info!("Registered provider: openrouter ({})", or_cfg.base_url);
            }
        }

        // ── Custom ──
        if let Some(ref custom_cfg) = config.agent.providers.custom {
            let provider = Arc::new(OpenAICompatProvider::new(
                &custom_cfg.base_url,
                custom_cfg.api_key.clone(),
                None,
                "custom",
            ));
            providers.insert("custom".into(), provider);
            info!("Registered provider: custom ({})", custom_cfg.base_url);
        }

        // If no Ollama provider was explicitly configured, register a default one
        // so local-only usage works out of the box.
        if !providers.contains_key("ollama") {
            info!("Registering default Ollama provider (http://127.0.0.1:11434)");
            providers.insert(
                "ollama".into(),
                Arc::new(OllamaProvider::new("http://127.0.0.1:11434", None, None)),
            );
        }

        let (default_provider, _) = config.parse_model();

        Self {
            providers,
            default_provider: default_provider.to_string(),
        }
    }

    /// Get a provider by name (e.g., "ollama", "anthropic", "openai").
    pub fn get(&self, name: &str) -> Option<Arc<dyn ModelProvider>> {
        self.providers.get(name).cloned()
    }

    /// Resolve a model string like "anthropic/claude-sonnet-4" to the correct provider.
    pub fn resolve(&self, model: &str) -> Result<Arc<dyn ModelProvider>> {
        let provider_name = model
            .split_once('/')
            .map(|(p, _)| p)
            .unwrap_or(&self.default_provider);

        self.providers
            .get(provider_name)
            .cloned()
            .ok_or_else(|| {
                Error::Gateway(format!(
                    "No provider registered for '{provider_name}' (model: {model}). Available: {:?}",
                    self.providers.keys().collect::<Vec<_>>()
                ))
            })
    }

    /// Get the default provider.
    pub fn default_provider(&self) -> Option<Arc<dyn ModelProvider>> {
        self.providers.get(&self.default_provider).cloned()
    }

    /// List all registered provider names.
    pub fn provider_names(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// Complete a chat request, automatically routing to the correct provider.
    ///
    /// Retries up to 3 times on transient failures with exponential backoff.
    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let provider = self.resolve(&request.model)?;
        let label = request.model.clone();
        let retry_cfg = super::retry::RetryConfig::default();

        super::retry::retry_with_backoff(&retry_cfg, &label, || {
            let req = request.clone();
            let prov = provider.clone();
            async move { prov.chat_completion(req).await }
        })
        .await
    }

    /// List all models from all providers.
    pub async fn list_all_models(&self) -> Vec<(String, Vec<ModelInfo>)> {
        let mut results = Vec::new();
        for (name, provider) in &self.providers {
            match provider.list_models().await {
                Ok(models) => results.push((name.clone(), models)),
                Err(e) => {
                    warn!(provider = %name, error = %e, "Failed to list models");
                    results.push((name.clone(), vec![]));
                }
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_registers_default_ollama() {
        let config = NgenOrcaConfig::default();
        let registry = ProviderRegistry::from_config(&config);
        assert!(registry.get("ollama").is_some());
    }

    #[test]
    fn resolve_finds_provider_by_model_prefix() {
        let config = NgenOrcaConfig::default();
        let registry = ProviderRegistry::from_config(&config);
        let p = registry.resolve("ollama/llama3").unwrap();
        assert_eq!(p.provider_name(), "ollama");
    }

    #[test]
    fn resolve_unknown_provider_errors() {
        let config = NgenOrcaConfig::default();
        let registry = ProviderRegistry::from_config(&config);
        assert!(registry.resolve("nonexistent/model").is_err());
    }

    #[test]
    fn provider_names_lists_all() {
        let config = NgenOrcaConfig::default();
        let registry = ProviderRegistry::from_config(&config);
        let names = registry.provider_names();
        assert!(names.contains(&"ollama"));
    }

    #[test]
    fn with_anthropic_config() {
        let mut config = NgenOrcaConfig::default();
        config.agent.providers.anthropic = Some(ngenorca_config::AnthropicProviderConfig {
            api_key: Some("sk-test".into()),
            base_url: "https://api.anthropic.com".into(),
            max_tokens: None,
            temperature: None,
        });
        let registry = ProviderRegistry::from_config(&config);
        assert!(registry.get("anthropic").is_some());
        assert!(registry.get("ollama").is_some()); // still registered
    }

    #[test]
    fn anthropic_without_key_skipped() {
        let mut config = NgenOrcaConfig::default();
        config.agent.providers.anthropic = Some(ngenorca_config::AnthropicProviderConfig {
            api_key: None,
            base_url: "https://api.anthropic.com".into(),
            max_tokens: None,
            temperature: None,
        });
        let registry = ProviderRegistry::from_config(&config);
        assert!(registry.get("anthropic").is_none());
    }
}
