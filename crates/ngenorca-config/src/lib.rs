//! # NgenOrca Config
//!
//! Composable configuration system. Instead of one monolithic config file,
//! NgenOrca merges config fragments from:
//!
//! 1. Built-in defaults
//! 2. System config (`/etc/ngenorca/` or `%PROGRAMDATA%\ngenorca\`)
//! 3. User config (`~/.ngenorca/config.toml`)
//! 4. Plugin-specific configs (`~/.ngenorca/plugins/<name>/config.toml`)
//! 5. Environment variables (`NGENORCA_*`)
//! 6. CLI flags
//!
//! Later sources override earlier ones. Each plugin declares its own config
//! schema, which is validated at load time.

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

/// Redact a secret for safe Debug output.
/// Shows `"[REDACTED len=N]"` if present, `"None"` if absent.
fn redact_option(secret: &Option<String>) -> String {
    match secret {
        Some(s) => format!("[REDACTED len={}]", s.len()),
        None => "None".into(),
    }
}

/// Redact a required secret for safe Debug output.
#[allow(dead_code)]
fn redact(secret: &str) -> String {
    format!("[REDACTED len={}]", secret.len())
}

/// Redact a Vec of secrets.
fn redact_vec(secrets: &[String]) -> String {
    format!("[{} tokens REDACTED]", secrets.len())
}

/// Top-level NgenOrca configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NgenOrcaConfig {
    /// Gateway settings.
    #[serde(default)]
    pub gateway: GatewayConfig,

    /// Agent/model settings.
    #[serde(default)]
    pub agent: AgentConfig,

    /// Channel adapter settings.
    #[serde(default)]
    pub channels: ChannelsConfig,

    /// Identity settings.
    #[serde(default)]
    pub identity: IdentityConfig,

    /// Memory settings.
    #[serde(default)]
    pub memory: MemoryConfig,

    /// Sandbox settings.
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Observability settings.
    #[serde(default)]
    pub observability: ObservabilityConfig,

    /// Data directory path.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
}

// ─── Gateway ────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Address to bind the gateway to.
    #[serde(default = "default_bind")]
    pub bind: String,

    /// Port for the gateway WebSocket + HTTP.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Authentication mode.
    #[serde(default)]
    pub auth_mode: AuthMode,

    /// Trusted reverse-proxy header name for the authenticated user.
    /// When behind a reverse proxy that does auth (e.g. Authelia + nginx),
    /// this header carries the verified username.
    #[serde(default = "default_proxy_user_header")]
    pub proxy_user_header: String,

    /// Trusted reverse-proxy header name for the user's email.
    #[serde(default = "default_proxy_email_header")]
    pub proxy_email_header: String,

    /// Trusted reverse-proxy header name for the user's groups.
    #[serde(default = "default_proxy_groups_header")]
    pub proxy_groups_header: String,

    /// Password for Password auth mode.
    #[serde(default)]
    pub auth_password: Option<String>,

    /// Tokens for Token auth mode.
    #[serde(default)]
    pub auth_tokens: Vec<String>,

    /// Path to TLS certificate (for Certificate or TLS modes).
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,

    /// Path to TLS private key.
    #[serde(default)]
    pub tls_key: Option<PathBuf>,

    /// Path to CA certificate for mTLS client verification.
    #[serde(default)]
    pub tls_ca: Option<PathBuf>,

    /// Rate limit: max requests per window per user (0 = disabled).
    #[serde(default = "default_rate_limit_max")]
    pub rate_limit_max: u32,

    /// Rate limit window in seconds.
    #[serde(default = "default_rate_limit_window_secs")]
    pub rate_limit_window_secs: u64,

    /// Session time-to-live in seconds (default: 7200 = 2 hours).
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,

    /// How often to prune expired sessions, in seconds (default: 300 = 5 min).
    #[serde(default = "default_session_prune_interval_secs")]
    pub session_prune_interval_secs: u64,

    /// Event log retention in days (default: 7).
    #[serde(default = "default_event_log_retention_days")]
    pub event_log_retention_days: u64,

    /// How often to prune the event log, in seconds (default: 21600 = 6 hours).
    #[serde(default = "default_event_log_prune_interval_secs")]
    pub event_log_prune_interval_secs: u64,

    /// CORS allowed origins. Empty = permissive (allow all).
    /// Example: ["https://my-ui.example.com", "http://localhost:3000"]
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,

    /// Maximum chat message length in characters (0 = unlimited, default: 32768).
    #[serde(default = "default_max_message_length")]
    pub max_message_length: usize,

    /// SEC-03: Allowed source IPs for TrustedProxy mode.
    ///
    /// When `auth_mode = "TrustedProxy"`, identity headers (`Remote-User`, etc.)
    /// are only trusted when the TCP connection originates from one of these IPs
    /// or CIDR ranges (e.g. `"10.0.0.0/8"`, `"172.16.0.0/12"`).
    /// Connections from any other source are rejected with 403.
    ///
    /// Default: `["127.0.0.1", "::1"]` (loopback only).
    #[serde(default = "default_trusted_proxy_sources")]
    pub trusted_proxy_sources: Vec<String>,
}

impl std::fmt::Debug for GatewayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayConfig")
            .field("bind", &self.bind)
            .field("port", &self.port)
            .field("auth_mode", &self.auth_mode)
            .field("proxy_user_header", &self.proxy_user_header)
            .field("proxy_email_header", &self.proxy_email_header)
            .field("proxy_groups_header", &self.proxy_groups_header)
            .field("auth_password", &redact_option(&self.auth_password))
            .field("auth_tokens", &redact_vec(&self.auth_tokens))
            .field("tls_cert", &self.tls_cert)
            .field("tls_key", &self.tls_key)
            .field("tls_ca", &self.tls_ca)
            .field("rate_limit_max", &self.rate_limit_max)
            .field("rate_limit_window_secs", &self.rate_limit_window_secs)
            .field("session_ttl_secs", &self.session_ttl_secs)
            .field(
                "session_prune_interval_secs",
                &self.session_prune_interval_secs,
            )
            .field("event_log_retention_days", &self.event_log_retention_days)
            .field(
                "event_log_prune_interval_secs",
                &self.event_log_prune_interval_secs,
            )
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("max_message_length", &self.max_message_length)
            .field("trusted_proxy_sources", &self.trusted_proxy_sources)
            .finish()
    }
}

// ─── Agent / LLM Providers ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Default model to use (e.g., "anthropic/claude-sonnet-4", "ollama/llama3.1").
    #[serde(default = "default_model")]
    pub model: String,

    /// Default thinking level.
    #[serde(default)]
    pub thinking_level: ngenorca_core::session::ThinkingLevel,

    /// Workspace root for agent files.
    #[serde(default = "default_workspace")]
    pub workspace: PathBuf,

    /// LLM provider configurations.
    #[serde(default)]
    pub providers: ProvidersConfig,

    /// Routing strategy for multi-agent orchestration.
    #[serde(default)]
    pub routing: RoutingStrategy,

    /// Lightweight classifier model for intent detection.
    /// Used before the main LLM when routing = Hybrid.
    #[serde(default)]
    pub classifier: Option<ClassifierConfig>,

    /// Quality gate configuration.
    #[serde(default)]
    pub quality_gate: QualityGateConfig,

    /// Sub-agents available for task delegation.
    #[serde(default)]
    pub sub_agents: Vec<SubAgentConfig>,
}

/// Configuration for all LLM providers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvidersConfig {
    /// Anthropic (Claude) provider.
    #[serde(default)]
    pub anthropic: Option<AnthropicProviderConfig>,

    /// OpenAI (GPT) provider.
    #[serde(default)]
    pub openai: Option<OpenAIProviderConfig>,

    /// Ollama (local models) provider.
    #[serde(default)]
    pub ollama: Option<OllamaProviderConfig>,

    /// Azure OpenAI provider.
    #[serde(default)]
    pub azure: Option<AzureProviderConfig>,

    /// Google Gemini provider.
    #[serde(default)]
    pub google: Option<GoogleProviderConfig>,

    /// OpenRouter (multi-provider) provider.
    #[serde(default)]
    pub openrouter: Option<OpenRouterProviderConfig>,

    /// Kilo Gateway provider.
    #[serde(default, alias = "kilocode")]
    pub kilo: Option<KiloProviderConfig>,

    /// Custom / self-hosted OpenAI-compatible provider.
    #[serde(default)]
    pub custom: Option<CustomProviderConfig>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AnthropicProviderConfig {
    /// Anthropic API key (prefer env var NGENORCA_AGENT__PROVIDERS__ANTHROPIC__API_KEY).
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL (default: https://api.anthropic.com).
    #[serde(default = "default_anthropic_url")]
    pub base_url: String,

    /// Anthropic API version header (default: "2023-06-01").
    #[serde(default = "default_anthropic_api_version")]
    pub api_version: String,

    /// Maximum tokens to generate.
    #[serde(default)]
    pub max_tokens: Option<usize>,

    /// Sampling temperature (0.0 - 1.0).
    #[serde(default)]
    pub temperature: Option<f64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OpenAIProviderConfig {
    /// OpenAI API key (prefer env var).
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL (default: https://api.openai.com/v1).
    #[serde(default = "default_openai_url")]
    pub base_url: String,

    /// Organization ID (optional).
    #[serde(default)]
    pub organization: Option<String>,

    /// Maximum tokens to generate.
    #[serde(default)]
    pub max_tokens: Option<usize>,

    /// Sampling temperature.
    #[serde(default)]
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaProviderConfig {
    /// Ollama API URL (default: http://127.0.0.1:11434).
    #[serde(default = "default_ollama_url")]
    pub base_url: String,

    /// How long to keep model in VRAM after last request.
    #[serde(default)]
    pub keep_alive: Option<String>,

    /// Context window size (affects VRAM usage).
    #[serde(default)]
    pub num_ctx: Option<usize>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AzureProviderConfig {
    /// Azure API key.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Azure OpenAI endpoint (e.g., https://your-resource.openai.azure.com).
    #[serde(default)]
    pub endpoint: Option<String>,

    /// Azure API version.
    #[serde(default = "default_azure_api_version")]
    pub api_version: String,

    /// Deployment name in Azure portal.
    #[serde(default)]
    pub deployment: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GoogleProviderConfig {
    /// Google AI API key.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL (default: https://generativelanguage.googleapis.com/v1beta).
    #[serde(default = "default_google_url")]
    pub base_url: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OpenRouterProviderConfig {
    /// OpenRouter API key.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL (default: https://openrouter.ai/api/v1).
    #[serde(default = "default_openrouter_url")]
    pub base_url: String,

    /// Site name shown in OpenRouter dashboard.
    #[serde(default)]
    pub site_name: Option<String>,

    /// Fallback models for auto-failover.
    #[serde(default)]
    pub fallback_models: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KiloProviderConfig {
    /// Kilo Gateway API key.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL (default: https://api.kilo.ai/api/gateway).
    #[serde(default = "default_kilo_url")]
    pub base_url: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CustomProviderConfig {
    /// Base URL for any OpenAI-compatible API (vLLM, LM Studio, LocalAI, etc.).
    pub base_url: String,

    /// API key (some servers require a dummy key).
    #[serde(default)]
    pub api_key: Option<String>,

    /// Model name the server expects.
    #[serde(default)]
    pub model_name: Option<String>,
}

// ─── Provider Debug impls (redact API keys) ─────────────────────

impl std::fmt::Debug for AnthropicProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProviderConfig")
            .field("api_key", &redact_option(&self.api_key))
            .field("base_url", &self.base_url)
            .field("api_version", &self.api_version)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .finish()
    }
}

impl std::fmt::Debug for OpenAIProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIProviderConfig")
            .field("api_key", &redact_option(&self.api_key))
            .field("base_url", &self.base_url)
            .field("organization", &self.organization)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .finish()
    }
}

impl std::fmt::Debug for AzureProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureProviderConfig")
            .field("api_key", &redact_option(&self.api_key))
            .field("endpoint", &self.endpoint)
            .field("api_version", &self.api_version)
            .field("deployment", &self.deployment)
            .finish()
    }
}

impl std::fmt::Debug for GoogleProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleProviderConfig")
            .field("api_key", &redact_option(&self.api_key))
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl std::fmt::Debug for OpenRouterProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouterProviderConfig")
            .field("api_key", &redact_option(&self.api_key))
            .field("base_url", &self.base_url)
            .field("site_name", &self.site_name)
            .field("fallback_models", &self.fallback_models)
            .finish()
    }
}

impl std::fmt::Debug for KiloProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiloProviderConfig")
            .field("api_key", &redact_option(&self.api_key))
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl std::fmt::Debug for CustomProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomProviderConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &redact_option(&self.api_key))
            .field("model_name", &self.model_name)
            .finish()
    }
}

// ─── Multi-Agent Orchestration ───────────────────────────────────

/// Routing strategy for the agent orchestrator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingStrategy {
    /// No sub-agents — all requests go to the primary model.
    #[default]
    Single,
    /// The orchestrator LLM decides which sub-agent to use.
    LlmRouted,
    /// Static rules based on intent → sub-agent mapping.
    RuleBased,
    /// Try local SLM first; escalate to cloud LLM if quality is insufficient.
    LocalFirst,
    /// Minimise cost: always use the cheapest model that can handle the task.
    CostOptimized,
    /// Hybrid: regex rules → SLM classifier → LLM (cascading, recommended).
    Hybrid,
}

/// Configuration for the lightweight intent classifier.
/// A small/fast model used to classify tasks before routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifierConfig {
    /// Model to use for classification (e.g., "ollama/qwen2.5:1.5b").
    pub model: String,

    /// Minimum confidence to trust the classifier (0.0–1.0).
    /// Below this, falls back to LLM-based classification.
    #[serde(default = "default_classifier_confidence")]
    pub confidence_threshold: f64,

    /// Maximum tokens for classification response.
    #[serde(default = "default_classifier_max_tokens")]
    pub max_tokens: usize,

    /// Temperature for classifier (low = deterministic).
    #[serde(default = "default_classifier_temperature")]
    pub temperature: f64,
}

/// Configuration for the quality gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityGateConfig {
    /// Enable quality gate evaluation of sub-agent responses.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Method: "heuristic", "slm", "llm", "auto".
    /// - heuristic: length/format checks only (zero cost)
    /// - slm: use classifier model to evaluate
    /// - llm: use primary model to evaluate (expensive)
    /// - auto: heuristic first, slm if uncertain
    #[serde(default = "default_quality_method")]
    pub method: String,

    /// Minimum response length (characters) to accept.
    #[serde(default = "default_min_response_length")]
    pub min_response_length: usize,

    /// Maximum retries before escalating.
    #[serde(default = "default_max_escalations")]
    pub max_escalations: u32,

    /// Auto-accept responses from high-confidence learned routing rules.
    #[serde(default = "default_true")]
    pub auto_accept_learned: bool,
}

impl Default for QualityGateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            method: default_quality_method(),
            min_response_length: default_min_response_length(),
            max_escalations: default_max_escalations(),
            auto_accept_learned: true,
        }
    }
}

/// Configuration for a sub-agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentConfig {
    /// Unique name for this sub-agent (e.g., "local-general", "coder").
    pub name: String,

    /// Model to use (e.g., "ollama/llama3.1:8b", "ollama/codellama:13b").
    pub model: String,

    /// Roles this sub-agent can fulfil.
    #[serde(default)]
    pub roles: Vec<String>,

    /// Base system prompt (the orchestrator may augment it dynamically).
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Maximum tokens to generate.
    #[serde(default = "default_sub_agent_max_tokens")]
    pub max_tokens: usize,

    /// Sampling temperature.
    #[serde(default = "default_sub_agent_temperature")]
    pub temperature: f64,

    /// Maximum complexity this sub-agent should handle.
    /// Tasks above this complexity will be routed elsewhere.
    #[serde(default = "default_max_complexity")]
    pub max_complexity: String,

    /// Whether this model is local (affects cost/privacy routing decisions).
    #[serde(default)]
    pub is_local: bool,

    /// Relative cost weight (1 = cheapest, 10 = most expensive).
    /// Used by CostOptimized routing.
    #[serde(default = "default_cost_weight")]
    pub cost_weight: u32,

    /// Priority (lower = preferred when multiple sub-agents match).
    #[serde(default = "default_priority")]
    pub priority: u32,
}

// ─── Channel Adapters ───────────────────────────────────────────

/// Channel adapter configurations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    /// Built-in WebChat (served by the gateway).
    #[serde(default)]
    pub webchat: Option<WebChatChannelConfig>,

    /// Telegram Bot adapter.
    #[serde(default)]
    pub telegram: Option<TelegramChannelConfig>,

    /// Discord Bot adapter.
    #[serde(default)]
    pub discord: Option<DiscordChannelConfig>,

    /// WhatsApp Business Cloud API adapter.
    #[serde(default)]
    pub whatsapp: Option<WhatsAppChannelConfig>,

    /// Slack Bot adapter.
    #[serde(default)]
    pub slack: Option<SlackChannelConfig>,

    /// Signal adapter (via signal-cli).
    #[serde(default)]
    pub signal: Option<SignalChannelConfig>,

    /// Matrix adapter.
    #[serde(default)]
    pub matrix: Option<MatrixChannelConfig>,

    /// Microsoft Teams adapter.
    #[serde(default)]
    pub teams: Option<TeamsChannelConfig>,

    /// Additional custom channels.
    #[serde(default)]
    pub custom: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebChatChannelConfig {
    /// Enable WebChat (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// UI theme.
    #[serde(default = "default_webchat_theme")]
    pub theme: String,

    /// Max upload size in MB.
    #[serde(default = "default_upload_size")]
    pub max_upload_size_mb: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    /// Enable Telegram adapter.
    #[serde(default)]
    pub enabled: bool,

    /// Bot token from @BotFather.
    #[serde(default)]
    pub bot_token: Option<String>,

    /// Use webhook mode (requires public URL). If false, uses long-polling.
    #[serde(default)]
    pub webhook_url: Option<String>,

    /// Use long-polling instead of webhooks (recommended for homelab/VPN setups).
    #[serde(default = "default_true")]
    pub polling: bool,

    /// Restrict to specific Telegram user IDs (empty = allow all).
    #[serde(default)]
    pub allowed_users: Vec<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DiscordChannelConfig {
    /// Enable Discord adapter.
    #[serde(default)]
    pub enabled: bool,

    /// Discord bot token.
    #[serde(default)]
    pub bot_token: Option<String>,

    /// Application ID.
    #[serde(default)]
    pub application_id: Option<String>,

    /// Restrict to specific guild (server) IDs.
    #[serde(default)]
    pub guild_ids: Vec<String>,

    /// Restrict to specific role names.
    #[serde(default)]
    pub allowed_roles: Vec<String>,

    /// Text command prefix (e.g., "!").
    #[serde(default)]
    pub command_prefix: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WhatsAppChannelConfig {
    /// Enable WhatsApp adapter.
    #[serde(default)]
    pub enabled: bool,

    /// Phone Number ID from Meta developer dashboard (Cloud API only, optional).
    #[serde(default)]
    pub phone_number_id: Option<String>,

    /// Permanent access token (Cloud API only, optional).
    #[serde(default)]
    pub access_token: Option<String>,

    /// Webhook verification token (Cloud API only, optional).
    #[serde(default)]
    pub verify_token: Option<String>,

    /// Webhook path the gateway listens on (Cloud API only).
    #[serde(default = "default_whatsapp_webhook_path")]
    pub webhook_path: String,

    /// App secret for webhook signature verification (Cloud API only).
    #[serde(default)]
    pub app_secret: Option<String>,

    /// Data directory for WhatsApp session/credential storage.
    /// Defaults to `~/.ngenorca/whatsapp-data` if not set.
    /// Used by the native pure-Rust client (default mode).
    #[serde(default)]
    pub data_path: Option<std::path::PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SlackChannelConfig {
    /// Enable Slack adapter.
    #[serde(default)]
    pub enabled: bool,

    /// Bot User OAuth Token.
    #[serde(default)]
    pub bot_token: Option<String>,

    /// App-Level Token (for Socket Mode).
    #[serde(default)]
    pub app_token: Option<String>,

    /// Signing secret for webhook verification.
    #[serde(default)]
    pub signing_secret: Option<String>,

    /// Use Socket Mode (no public URL needed).
    #[serde(default = "default_true")]
    pub socket_mode: bool,

    /// Restrict to specific channel IDs.
    #[serde(default)]
    pub channel_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalChannelConfig {
    /// Enable Signal adapter.
    #[serde(default)]
    pub enabled: bool,

    /// Signal phone number.
    #[serde(default)]
    pub phone_number: Option<String>,

    /// Path to signal-cli binary.
    #[serde(default)]
    pub signal_cli_path: Option<PathBuf>,

    /// signal-cli data path.
    #[serde(default)]
    pub data_path: Option<PathBuf>,

    /// Signal-cli mode (daemon or dbus).
    #[serde(default = "default_signal_mode")]
    pub mode: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MatrixChannelConfig {
    /// Enable Matrix adapter.
    #[serde(default)]
    pub enabled: bool,

    /// Matrix homeserver URL.
    #[serde(default)]
    pub homeserver: Option<String>,

    /// Bot user ID (e.g., @ngenorca:matrix.org).
    #[serde(default)]
    pub user_id: Option<String>,

    /// Access token.
    #[serde(default)]
    pub access_token: Option<String>,

    /// Device ID.
    #[serde(default)]
    pub device_id: Option<String>,

    /// Auto-join rooms when invited.
    #[serde(default)]
    pub auto_join: bool,

    /// Enable end-to-end encryption.
    #[serde(default)]
    pub encrypted: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TeamsChannelConfig {
    /// Enable Microsoft Teams adapter.
    #[serde(default)]
    pub enabled: bool,

    /// Bot Framework App ID.
    #[serde(default)]
    pub app_id: Option<String>,

    /// Bot Framework App Password.
    #[serde(default)]
    pub app_password: Option<String>,

    /// Azure AD Tenant ID ("common" for multi-tenant).
    #[serde(default = "default_teams_tenant")]
    pub tenant_id: String,

    /// Webhook URL for incoming messages.
    #[serde(default)]
    pub webhook_url: Option<String>,
}

// ─── Channel Debug impls (redact tokens/secrets) ────────────────

impl std::fmt::Debug for TelegramChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramChannelConfig")
            .field("enabled", &self.enabled)
            .field("bot_token", &redact_option(&self.bot_token))
            .field("webhook_url", &self.webhook_url)
            .field("polling", &self.polling)
            .field("allowed_users", &self.allowed_users)
            .finish()
    }
}

impl std::fmt::Debug for DiscordChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordChannelConfig")
            .field("enabled", &self.enabled)
            .field("bot_token", &redact_option(&self.bot_token))
            .field("application_id", &self.application_id)
            .field("guild_ids", &self.guild_ids)
            .field("allowed_roles", &self.allowed_roles)
            .field("command_prefix", &self.command_prefix)
            .finish()
    }
}

impl std::fmt::Debug for WhatsAppChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhatsAppChannelConfig")
            .field("enabled", &self.enabled)
            .field("phone_number_id", &self.phone_number_id)
            .field("access_token", &redact_option(&self.access_token))
            .field("verify_token", &redact_option(&self.verify_token))
            .field("webhook_path", &self.webhook_path)
            .field("app_secret", &redact_option(&self.app_secret))
            .field("data_path", &self.data_path)
            .finish()
    }
}

impl std::fmt::Debug for SlackChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackChannelConfig")
            .field("enabled", &self.enabled)
            .field("bot_token", &redact_option(&self.bot_token))
            .field("app_token", &redact_option(&self.app_token))
            .field("signing_secret", &redact_option(&self.signing_secret))
            .field("socket_mode", &self.socket_mode)
            .field("channel_ids", &self.channel_ids)
            .finish()
    }
}

impl std::fmt::Debug for MatrixChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixChannelConfig")
            .field("enabled", &self.enabled)
            .field("homeserver", &self.homeserver)
            .field("user_id", &self.user_id)
            .field("access_token", &redact_option(&self.access_token))
            .field("device_id", &self.device_id)
            .field("auto_join", &self.auto_join)
            .field("encrypted", &self.encrypted)
            .finish()
    }
}

impl std::fmt::Debug for TeamsChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamsChannelConfig")
            .field("enabled", &self.enabled)
            .field("app_id", &self.app_id)
            .field("app_password", &redact_option(&self.app_password))
            .field("tenant_id", &self.tenant_id)
            .field("webhook_url", &self.webhook_url)
            .finish()
    }
}

// ─── Identity ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityConfig {
    /// Whether to require hardware attestation for owner identity.
    #[serde(default = "default_true")]
    pub require_hardware_attestation: bool,

    /// Whether to enable behavioral biometrics (voice print, typing cadence).
    #[serde(default)]
    pub biometrics_enabled: bool,

    /// Auto-lock after N minutes of inactivity (0 = disabled).
    #[serde(default = "default_auto_lock_minutes")]
    pub auto_lock_minutes: u32,
}

// ─── Memory ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Enable three-tier memory system.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Max episodic memory entries before pruning.
    #[serde(default = "default_episodic_max")]
    pub episodic_max_entries: usize,

    /// Consolidation interval (seconds).
    #[serde(default = "default_consolidation_interval")]
    pub consolidation_interval_secs: u64,

    /// Max semantic memory tokens to inject per prompt.
    #[serde(default = "default_semantic_budget")]
    pub semantic_token_budget: usize,
}

// ─── Sandbox ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Sandbox backend (auto-detected if not set).
    #[serde(default)]
    pub backend: SandboxBackend,

    /// Whether tool execution is sandboxed by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum SandboxBackend {
    /// Auto-detect based on environment.
    #[default]
    Auto,
    /// Windows Job Objects + Restricted Tokens.
    WindowsJob,
    /// Linux seccomp + landlock + namespaces.
    LinuxSeccomp,
    /// macOS App Sandbox.
    MacOsSandbox,
    /// Container-level (defer to Docker/Podman).
    Container,
    /// Disabled (not recommended).
    None,
}

// ─── Auth Modes ─────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum AuthMode {
    /// No authentication (loopback only or trusted proxy).
    #[default]
    None,
    /// Shared password.
    Password,
    /// Token-based.
    Token,
    /// mTLS client certificates.
    Certificate,
    /// Trusted reverse proxy (Authelia, Authentik, etc.).
    /// The proxy handles authentication; NgenOrca reads the verified
    /// user from the `proxy_user_header` header.
    TrustedProxy,
}

// ─── Observability ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Enable OpenTelemetry export.
    #[serde(default)]
    pub otlp_enabled: bool,

    /// OTLP endpoint (e.g., "http://localhost:4317").
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,

    /// Log level filter.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Enable JSON structured logging.
    #[serde(default)]
    pub json_logs: bool,
}

// ─── Default value functions ────────────────────────────────────

fn default_bind() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    18789
}
fn default_model() -> String {
    "anthropic/claude-sonnet-4-20250514".into()
}
fn default_data_dir() -> PathBuf {
    dirs_default().join("data")
}
fn default_workspace() -> PathBuf {
    dirs_default().join("workspace")
}
fn default_true() -> bool {
    true
}
fn default_auto_lock_minutes() -> u32 {
    30
}
fn default_episodic_max() -> usize {
    100_000
}
fn default_consolidation_interval() -> u64 {
    3600
}
fn default_semantic_budget() -> usize {
    4096
}
fn default_otlp_endpoint() -> String {
    "http://localhost:4317".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_proxy_user_header() -> String {
    "Remote-User".into()
}
fn default_proxy_email_header() -> String {
    "Remote-Email".into()
}
fn default_proxy_groups_header() -> String {
    "Remote-Groups".into()
}
fn default_anthropic_url() -> String {
    "https://api.anthropic.com".into()
}
fn default_anthropic_api_version() -> String {
    "2023-06-01".into()
}
fn default_openai_url() -> String {
    "https://api.openai.com/v1".into()
}
fn default_ollama_url() -> String {
    "http://127.0.0.1:11434".into()
}
fn default_azure_api_version() -> String {
    "2024-10-21".into()
}
fn default_google_url() -> String {
    "https://generativelanguage.googleapis.com/v1beta".into()
}
fn default_openrouter_url() -> String {
    "https://openrouter.ai/api/v1".into()
}
fn default_kilo_url() -> String {
    "https://api.kilo.ai/api/gateway".into()
}
fn default_classifier_confidence() -> f64 {
    0.8
}
fn default_classifier_max_tokens() -> usize {
    64
}
fn default_classifier_temperature() -> f64 {
    0.1
}
fn default_quality_method() -> String {
    "auto".into()
}
fn default_min_response_length() -> usize {
    10
}
fn default_max_escalations() -> u32 {
    2
}
fn default_sub_agent_max_tokens() -> usize {
    2048
}
fn default_sub_agent_temperature() -> f64 {
    0.3
}
fn default_max_complexity() -> String {
    "Moderate".into()
}
fn default_cost_weight() -> u32 {
    1
}
fn default_priority() -> u32 {
    10
}
fn default_rate_limit_max() -> u32 {
    60
}
fn default_rate_limit_window_secs() -> u64 {
    60
}
fn default_session_ttl_secs() -> u64 {
    7200
}
fn default_session_prune_interval_secs() -> u64 {
    300
}
fn default_event_log_retention_days() -> u64 {
    7
}
fn default_event_log_prune_interval_secs() -> u64 {
    21_600
}
fn default_max_message_length() -> usize {
    32_768
}
fn default_trusted_proxy_sources() -> Vec<String> {
    vec!["127.0.0.1".into(), "::1".into()]
}
fn default_webchat_theme() -> String {
    "dark".into()
}
fn default_upload_size() -> usize {
    10
}
fn default_whatsapp_webhook_path() -> String {
    "/webhooks/whatsapp".into()
}
fn default_signal_mode() -> String {
    "daemon".into()
}
fn default_teams_tenant() -> String {
    "common".into()
}

fn dirs_default() -> PathBuf {
    dirs_home().join(".ngenorca")
}

pub fn default_user_config_path() -> PathBuf {
    dirs_default().join("config.toml")
}

pub fn default_system_config_path() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:\\ProgramData"))
            .join("ngenorca")
            .join("config.toml")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/ngenorca/config.toml")
    }
}

pub fn resolve_config_path(config_path: Option<&str>) -> PathBuf {
    config_path
        .map(PathBuf::from)
        .unwrap_or_else(default_user_config_path)
}

fn dirs_home() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:\\Users\\default"))
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
}

// ─── Default impls ──────────────────────────────────────────────

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: default_port(),
            auth_mode: AuthMode::default(),
            proxy_user_header: default_proxy_user_header(),
            proxy_email_header: default_proxy_email_header(),
            proxy_groups_header: default_proxy_groups_header(),
            auth_password: None,
            auth_tokens: vec![],
            tls_cert: None,
            tls_key: None,
            tls_ca: None,
            rate_limit_max: default_rate_limit_max(),
            rate_limit_window_secs: default_rate_limit_window_secs(),
            session_ttl_secs: default_session_ttl_secs(),
            session_prune_interval_secs: default_session_prune_interval_secs(),
            event_log_retention_days: default_event_log_retention_days(),
            event_log_prune_interval_secs: default_event_log_prune_interval_secs(),
            cors_allowed_origins: vec![],
            max_message_length: default_max_message_length(),
            trusted_proxy_sources: default_trusted_proxy_sources(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            thinking_level: ngenorca_core::session::ThinkingLevel::default(),
            workspace: default_workspace(),
            providers: ProvidersConfig::default(),
            routing: RoutingStrategy::default(),
            classifier: None,
            quality_gate: QualityGateConfig::default(),
            sub_agents: vec![],
        }
    }
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            require_hardware_attestation: true,
            biometrics_enabled: false,
            auto_lock_minutes: default_auto_lock_minutes(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            episodic_max_entries: default_episodic_max(),
            consolidation_interval_secs: default_consolidation_interval(),
            semantic_token_budget: default_semantic_budget(),
        }
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            backend: SandboxBackend::Auto,
            enabled: true,
        }
    }
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            otlp_enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            log_level: default_log_level(),
            json_logs: false,
        }
    }
}

impl Default for NgenOrcaConfig {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig::default(),
            agent: AgentConfig::default(),
            channels: ChannelsConfig::default(),
            identity: IdentityConfig::default(),
            memory: MemoryConfig::default(),
            sandbox: SandboxConfig::default(),
            observability: ObservabilityConfig::default(),
            data_dir: default_data_dir(),
        }
    }
}

// ─── Helper methods ─────────────────────────────────────────────

impl NgenOrcaConfig {
    /// Parse the model string to extract (provider, model_name).
    /// e.g., "anthropic/claude-sonnet-4" → ("anthropic", "claude-sonnet-4")
    pub fn parse_model(&self) -> (&str, &str) {
        self.agent
            .model
            .split_once('/')
            .unwrap_or(("custom", &self.agent.model))
    }

    /// Check if using a trusted reverse proxy for authentication.
    pub fn is_trusted_proxy(&self) -> bool {
        matches!(self.gateway.auth_mode, AuthMode::TrustedProxy)
    }

    /// Get the list of enabled channels.
    pub fn enabled_channels(&self) -> Vec<&str> {
        let mut channels = Vec::new();
        if self.channels.webchat.as_ref().is_some_and(|c| c.enabled) {
            channels.push("webchat");
        }
        if self.channels.telegram.as_ref().is_some_and(|c| c.enabled) {
            channels.push("telegram");
        }
        if self.channels.discord.as_ref().is_some_and(|c| c.enabled) {
            channels.push("discord");
        }
        if self.channels.whatsapp.as_ref().is_some_and(|c| c.enabled) {
            channels.push("whatsapp");
        }
        if self.channels.slack.as_ref().is_some_and(|c| c.enabled) {
            channels.push("slack");
        }
        if self.channels.signal.as_ref().is_some_and(|c| c.enabled) {
            channels.push("signal");
        }
        if self.channels.matrix.as_ref().is_some_and(|c| c.enabled) {
            channels.push("matrix");
        }
        if self.channels.teams.as_ref().is_some_and(|c| c.enabled) {
            channels.push("teams");
        }
        channels
    }

    /// Check if multi-agent orchestration is enabled.
    pub fn is_orchestrated(&self) -> bool {
        !matches!(self.agent.routing, RoutingStrategy::Single) && !self.agent.sub_agents.is_empty()
    }

    /// Get the list of configured sub-agent names.
    pub fn sub_agent_names(&self) -> Vec<&str> {
        self.agent
            .sub_agents
            .iter()
            .map(|s| s.name.as_str())
            .collect()
    }

    /// Find a sub-agent config by name.
    pub fn sub_agent(&self, name: &str) -> Option<&SubAgentConfig> {
        self.agent.sub_agents.iter().find(|s| s.name == name)
    }

    /// Get sub-agents that can handle a given role.
    pub fn sub_agents_for_role(&self, role: &str) -> Vec<&SubAgentConfig> {
        self.agent
            .sub_agents
            .iter()
            .filter(|s| s.roles.iter().any(|r| r.eq_ignore_ascii_case(role)))
            .collect()
    }

    /// Get the cheapest sub-agent (lowest cost_weight) that matches a role.
    pub fn cheapest_agent_for_role(&self, role: &str) -> Option<&SubAgentConfig> {
        self.sub_agents_for_role(role)
            .into_iter()
            .min_by_key(|s| s.cost_weight)
    }

    /// Validate the configuration, returning a list of problems.
    ///
    /// Returns `Ok(warnings)` when the config is usable (warnings are
    /// non-fatal hints) or `Err(errors)` when the config has fatal issues.
    pub fn validate(&self) -> std::result::Result<Vec<String>, Vec<String>> {
        let mut errors: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        // ── Gateway ──
        if self.gateway.port == 0 {
            errors.push("gateway.port must not be 0".into());
        }
        if self.gateway.bind.is_empty() {
            errors.push("gateway.bind must not be empty".into());
        }
        if self.gateway.rate_limit_window_secs == 0 && self.gateway.rate_limit_max > 0 {
            errors.push(
                "gateway.rate_limit_window_secs must be > 0 when rate limiting is enabled".into(),
            );
        }
        if self.gateway.session_ttl_secs == 0 {
            warnings.push("gateway.session_ttl_secs is 0 — sessions will never expire".into());
        }
        if self.gateway.session_prune_interval_secs == 0 {
            warnings.push("gateway.session_prune_interval_secs is 0 — session pruning will run in a tight loop".into());
        }
        if self.gateway.event_log_retention_days == 0 {
            warnings.push(
                "gateway.event_log_retention_days is 0 — event logs will be pruned immediately"
                    .into(),
            );
        }
        if self.gateway.event_log_prune_interval_secs == 0 {
            warnings.push("gateway.event_log_prune_interval_secs is 0 — event log pruning will run in a tight loop".into());
        }

        // Auth-mode-specific
        match &self.gateway.auth_mode {
            AuthMode::Password => {
                if self
                    .gateway
                    .auth_password
                    .as_ref()
                    .is_none_or(|p| p.is_empty())
                {
                    errors
                        .push("gateway.auth_password is required when auth_mode = Password".into());
                }
            }
            AuthMode::Token => {
                if self.gateway.auth_tokens.is_empty() {
                    errors.push("gateway.auth_tokens must contain at least one token when auth_mode = Token".into());
                }
            }
            AuthMode::Certificate => {
                if self.gateway.tls_cert.is_none() {
                    errors.push("gateway.tls_cert is required when auth_mode = Certificate".into());
                }
                if self.gateway.tls_key.is_none() {
                    errors.push("gateway.tls_key is required when auth_mode = Certificate".into());
                }
            }
            _ => {}
        }

        // ── Agent ──
        if self.agent.model.is_empty() {
            errors.push("agent.model must not be empty".into());
        }

        // Classifier thresholds
        if let Some(cls) = &self.agent.classifier {
            if !(0.0..=1.0).contains(&cls.confidence_threshold) {
                errors.push(format!(
                    "agent.classifier.confidence_threshold must be 0.0–1.0, got {}",
                    cls.confidence_threshold
                ));
            }
            if cls.temperature < 0.0 || cls.temperature > 2.0 {
                errors.push(format!(
                    "agent.classifier.temperature must be 0.0–2.0, got {}",
                    cls.temperature
                ));
            }
        }

        // Quality gate
        let valid_methods = ["heuristic", "slm", "llm", "auto"];
        if !valid_methods.contains(&self.agent.quality_gate.method.as_str()) {
            warnings.push(format!(
                "agent.quality_gate.method '{}' is not one of {:?}",
                self.agent.quality_gate.method, valid_methods
            ));
        }

        // Sub-agent uniqueness
        let mut seen_names = std::collections::HashSet::new();
        for sa in &self.agent.sub_agents {
            if !seen_names.insert(&sa.name) {
                errors.push(format!("duplicate sub_agent name: '{}'", sa.name));
            }
            if sa.cost_weight == 0 {
                warnings.push(format!(
                    "sub_agent '{}' has cost_weight 0 — it will always be cheapest",
                    sa.name
                ));
            }
        }

        // ── Channels ──
        if let Some(tg) = &self.channels.telegram
            && tg.enabled
            && tg.bot_token.as_ref().is_none_or(|t| t.is_empty())
        {
            errors.push("channels.telegram.bot_token is required when telegram is enabled".into());
        }
        if let Some(dc) = &self.channels.discord
            && dc.enabled
            && dc.bot_token.as_ref().is_none_or(|t| t.is_empty())
        {
            errors.push("channels.discord.bot_token is required when discord is enabled".into());
        }
        // WhatsApp: Native mode (no access_token) is always valid.
        // No special validation needed — native mode works with just enabled=true.
        if let Some(sl) = &self.channels.slack
            && sl.enabled
            && sl.bot_token.as_ref().is_none_or(|t| t.is_empty())
        {
            errors.push("channels.slack.bot_token is required when slack is enabled".into());
        }
        if let Some(sg) = &self.channels.signal
            && sg.enabled
            && sg.phone_number.as_ref().is_none_or(|t| t.is_empty())
        {
            errors.push("channels.signal.phone_number is required when signal is enabled".into());
        }
        if let Some(mx) = &self.channels.matrix
            && mx.enabled
            && mx.access_token.as_ref().is_none_or(|t| t.is_empty())
        {
            errors.push("channels.matrix.access_token is required when matrix is enabled".into());
        }
        if let Some(tm) = &self.channels.teams
            && tm.enabled
            && tm.app_id.as_ref().is_none_or(|t| t.is_empty())
        {
            errors.push("channels.teams.app_id is required when teams is enabled".into());
        }

        // ── Provider validation ──
        let url_fields: Vec<(&str, &str)> = vec![
            self.agent
                .providers
                .anthropic
                .as_ref()
                .map(|p| ("agent.providers.anthropic.base_url", p.base_url.as_str())),
            self.agent
                .providers
                .openai
                .as_ref()
                .map(|p| ("agent.providers.openai.base_url", p.base_url.as_str())),
            self.agent
                .providers
                .ollama
                .as_ref()
                .map(|p| ("agent.providers.ollama.base_url", p.base_url.as_str())),
            self.agent.providers.azure.as_ref().and_then(|p| {
                p.endpoint
                    .as_ref()
                    .map(|e| ("agent.providers.azure.endpoint", e.as_str()))
            }),
            self.agent
                .providers
                .google
                .as_ref()
                .map(|p| ("agent.providers.google.base_url", p.base_url.as_str())),
            self.agent
                .providers
                .openrouter
                .as_ref()
                .map(|p| ("agent.providers.openrouter.base_url", p.base_url.as_str())),
            self.agent
                .providers
                .custom
                .as_ref()
                .map(|p| ("agent.providers.custom.base_url", p.base_url.as_str())),
        ]
        .into_iter()
        .flatten()
        .collect();

        for (field, url) in &url_fields {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                errors.push(format!(
                    "{field} must start with http:// or https://, got \"{url}\""
                ));
            }
        }

        // Temperature range validation (0.0–2.0) for providers that set it
        if let Some(p) = &self.agent.providers.anthropic {
            if let Some(t) = p.temperature
                && !(0.0..=2.0).contains(&t)
            {
                errors.push(format!(
                    "agent.providers.anthropic.temperature must be 0.0–2.0, got {t}"
                ));
            }
            if p.max_tokens == Some(0) {
                errors.push("agent.providers.anthropic.max_tokens must be > 0".into());
            }
        }
        if let Some(p) = &self.agent.providers.openai {
            if let Some(t) = p.temperature
                && !(0.0..=2.0).contains(&t)
            {
                errors.push(format!(
                    "agent.providers.openai.temperature must be 0.0–2.0, got {t}"
                ));
            }
            if p.max_tokens == Some(0) {
                errors.push("agent.providers.openai.max_tokens must be > 0".into());
            }
        }

        // ── Memory ──
        if self.memory.consolidation_interval_secs == 0 {
            warnings.push(
                "memory.consolidation_interval_secs is 0 — consolidation will run in a tight loop"
                    .into(),
            );
        }

        if errors.is_empty() {
            Ok(warnings)
        } else {
            Err(errors)
        }
    }
}

/// Load configuration by merging all sources in priority order.
pub fn load_config(config_path: Option<&str>) -> ngenorca_core::Result<NgenOrcaConfig> {
    let mut figment = Figment::from(Serialized::defaults(NgenOrcaConfig::default()));

    let system_config = default_system_config_path();
    if system_config.exists() {
        info!(?system_config, "Loading system config file");
        figment = figment.merge(Toml::file(&system_config));
    }

    // User config file.
    let user_config = resolve_config_path(config_path);

    if user_config != system_config && user_config.exists() {
        info!(?user_config, "Loading config file");
        figment = figment.merge(Toml::file(&user_config));
    }

    // Environment variables (NGENORCA_GATEWAY__PORT=9999 etc.).
    figment = figment.merge(Env::prefixed("NGENORCA_").split("__"));

    let config: NgenOrcaConfig = figment
        .extract()
        .map_err(|e| ngenorca_core::Error::Config(e.to_string()))?;

    // Validate the configuration.
    match config.validate() {
        Ok(warnings) => {
            for w in &warnings {
                tracing::warn!("Config warning: {w}");
            }
        }
        Err(errors) => {
            for e in &errors {
                tracing::error!("Config error: {e}");
            }
            return Err(ngenorca_core::Error::Config(format!(
                "Configuration has {} error(s): {}",
                errors.len(),
                errors.join("; ")
            )));
        }
    }

    let (provider, model) = config.parse_model();
    let channels = config.enabled_channels();
    let sub_agents = config.sub_agent_names();
    info!(
        bind = %config.gateway.bind,
        port = %config.gateway.port,
        provider,
        model,
        auth = ?config.gateway.auth_mode,
        routing = ?config.agent.routing,
        ?channels,
        ?sub_agents,
        orchestrated = config.is_orchestrated(),
        "Configuration loaded"
    );

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_expected_bind_and_port() {
        let cfg = NgenOrcaConfig::default();
        assert_eq!(cfg.gateway.bind, "127.0.0.1");
        assert_eq!(cfg.gateway.port, 18789);
    }

    #[test]
    fn default_auth_mode_is_none() {
        let cfg = NgenOrcaConfig::default();
        assert!(matches!(cfg.gateway.auth_mode, AuthMode::None));
    }

    #[test]
    fn default_routing_is_single() {
        let cfg = NgenOrcaConfig::default();
        assert!(matches!(cfg.agent.routing, RoutingStrategy::Single));
    }

    #[test]
    fn parse_model_splits_provider_and_name() {
        let cfg = NgenOrcaConfig {
            agent: AgentConfig {
                model: "anthropic/claude-sonnet-4".into(),
                ..AgentConfig::default()
            },
            ..NgenOrcaConfig::default()
        };
        let (provider, model) = cfg.parse_model();
        assert_eq!(provider, "anthropic");
        assert_eq!(model, "claude-sonnet-4");
    }

    #[test]
    fn parse_model_without_slash_returns_custom() {
        let cfg = NgenOrcaConfig {
            agent: AgentConfig {
                model: "gpt-4o".into(),
                ..AgentConfig::default()
            },
            ..NgenOrcaConfig::default()
        };
        let (provider, model) = cfg.parse_model();
        assert_eq!(provider, "custom");
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn providers_kilocode_alias_deserializes_to_kilo() {
        let cfg: NgenOrcaConfig = toml::from_str(
            r#"
                [agent]
                model = "kilo/anthropic/claude-sonnet-4.5"

                [agent.providers.kilocode]
                api_key = "kgw_test"
            "#,
        )
        .unwrap();

        let kilo = cfg.agent.providers.kilo.expect("kilo config should deserialize");
        assert_eq!(kilo.api_key.as_deref(), Some("kgw_test"));
        assert_eq!(kilo.base_url, "https://api.kilo.ai/api/gateway");
    }

    #[test]
    fn is_trusted_proxy_true_when_set() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.gateway.auth_mode = AuthMode::TrustedProxy;
        assert!(cfg.is_trusted_proxy());
    }

    #[test]
    fn is_trusted_proxy_false_when_none() {
        let cfg = NgenOrcaConfig::default();
        assert!(!cfg.is_trusted_proxy());
    }

    #[test]
    fn enabled_channels_empty_by_default() {
        let cfg = NgenOrcaConfig::default();
        assert!(cfg.enabled_channels().is_empty());
    }

    #[test]
    fn enabled_channels_detects_webchat() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.channels.webchat = Some(WebChatChannelConfig {
            enabled: true,
            theme: "dark".into(),
            max_upload_size_mb: 10,
        });
        let channels = cfg.enabled_channels();
        assert_eq!(channels, vec!["webchat"]);
    }

    #[test]
    fn is_orchestrated_when_routing_and_agents() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.routing = RoutingStrategy::Hybrid;
        cfg.agent.sub_agents = vec![SubAgentConfig {
            name: "local".into(),
            model: "ollama/llama3".into(),
            roles: vec!["general".into()],
            system_prompt: None,
            max_tokens: 2048,
            temperature: 0.3,
            max_complexity: "Moderate".into(),
            is_local: true,
            cost_weight: 1,
            priority: 10,
        }];
        assert!(cfg.is_orchestrated());
    }

    #[test]
    fn is_not_orchestrated_with_single_routing() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.routing = RoutingStrategy::Single;
        cfg.agent.sub_agents = vec![SubAgentConfig {
            name: "local".into(),
            model: "ollama/llama3".into(),
            roles: vec!["general".into()],
            system_prompt: None,
            max_tokens: 2048,
            temperature: 0.3,
            max_complexity: "Moderate".into(),
            is_local: true,
            cost_weight: 1,
            priority: 10,
        }];
        assert!(!cfg.is_orchestrated());
    }

    #[test]
    fn is_not_orchestrated_with_no_agents() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.routing = RoutingStrategy::Hybrid;
        cfg.agent.sub_agents = vec![];
        assert!(!cfg.is_orchestrated());
    }

    fn sample_sub_agents() -> Vec<SubAgentConfig> {
        vec![
            SubAgentConfig {
                name: "local-general".into(),
                model: "ollama/llama3:8b".into(),
                roles: vec!["general".into(), "summarization".into()],
                system_prompt: None,
                max_tokens: 2048,
                temperature: 0.3,
                max_complexity: "Moderate".into(),
                is_local: true,
                cost_weight: 1,
                priority: 10,
            },
            SubAgentConfig {
                name: "coder".into(),
                model: "ollama/codellama:13b".into(),
                roles: vec!["coding".into()],
                system_prompt: Some("You are a coding expert.".into()),
                max_tokens: 4096,
                temperature: 0.2,
                max_complexity: "Complex".into(),
                is_local: true,
                cost_weight: 2,
                priority: 5,
            },
            SubAgentConfig {
                name: "cloud".into(),
                model: "anthropic/claude-sonnet-4".into(),
                roles: vec!["general".into(), "coding".into()],
                system_prompt: None,
                max_tokens: 8192,
                temperature: 0.7,
                max_complexity: "Expert".into(),
                is_local: false,
                cost_weight: 10,
                priority: 100,
            },
        ]
    }

    #[test]
    fn sub_agent_finds_by_name() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.sub_agents = sample_sub_agents();
        assert!(cfg.sub_agent("coder").is_some());
        assert_eq!(
            cfg.sub_agent("coder").unwrap().model,
            "ollama/codellama:13b"
        );
        assert!(cfg.sub_agent("nonexistent").is_none());
    }

    #[test]
    fn sub_agents_for_role_filters_correctly() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.sub_agents = sample_sub_agents();
        let general = cfg.sub_agents_for_role("general");
        assert_eq!(general.len(), 2); // local-general + cloud
        let coding = cfg.sub_agents_for_role("coding");
        assert_eq!(coding.len(), 2); // coder + cloud
        let empty = cfg.sub_agents_for_role("vision");
        assert!(empty.is_empty());
    }

    #[test]
    fn cheapest_agent_for_role_returns_min_cost() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.sub_agents = sample_sub_agents();
        let cheapest = cfg.cheapest_agent_for_role("general").unwrap();
        assert_eq!(cheapest.name, "local-general");
        assert_eq!(cheapest.cost_weight, 1);
    }

    #[test]
    fn cheapest_agent_for_role_returns_none_when_empty() {
        let cfg = NgenOrcaConfig::default();
        assert!(cfg.cheapest_agent_for_role("anything").is_none());
    }

    #[test]
    fn sub_agent_names_returns_all() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.sub_agents = sample_sub_agents();
        let names = cfg.sub_agent_names();
        assert_eq!(names, vec!["local-general", "coder", "cloud"]);
    }

    #[test]
    fn default_proxy_headers() {
        let cfg = NgenOrcaConfig::default();
        assert_eq!(cfg.gateway.proxy_user_header, "Remote-User");
        assert_eq!(cfg.gateway.proxy_email_header, "Remote-Email");
        assert_eq!(cfg.gateway.proxy_groups_header, "Remote-Groups");
    }

    #[test]
    fn quality_gate_defaults() {
        let qg = QualityGateConfig::default();
        assert!(qg.enabled);
        assert_eq!(qg.method, "auto");
        assert_eq!(qg.min_response_length, 10);
        assert_eq!(qg.max_escalations, 2);
        assert!(qg.auto_accept_learned);
    }

    #[test]
    fn auth_mode_serde_roundtrip() {
        let modes = vec![
            AuthMode::None,
            AuthMode::TrustedProxy,
            AuthMode::Token,
            AuthMode::Password,
            AuthMode::Certificate,
        ];
        for mode in &modes {
            let json = serde_json::to_string(mode).unwrap();
            let back: AuthMode = serde_json::from_str(&json).unwrap();
            // AuthMode doesn't derive PartialEq, so compare debug strings
            assert_eq!(format!("{:?}", mode), format!("{:?}", back));
        }
    }

    #[test]
    fn routing_strategy_serde_roundtrip() {
        let strategies = vec![
            RoutingStrategy::Single,
            RoutingStrategy::LlmRouted,
            RoutingStrategy::RuleBased,
            RoutingStrategy::LocalFirst,
            RoutingStrategy::CostOptimized,
            RoutingStrategy::Hybrid,
        ];
        for s in &strategies {
            let json = serde_json::to_string(s).unwrap();
            let back: RoutingStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(*s, back);
        }
    }

    #[test]
    fn config_serde_roundtrip() {
        let cfg = NgenOrcaConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: NgenOrcaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.gateway.port, back.gateway.port);
        assert_eq!(cfg.gateway.bind, back.gateway.bind);
        assert_eq!(cfg.agent.model, back.agent.model);
    }

    #[test]
    fn identity_config_defaults() {
        let cfg = IdentityConfig::default();
        assert!(cfg.require_hardware_attestation);
        assert!(!cfg.biometrics_enabled);
    }

    #[test]
    fn memory_config_defaults() {
        let cfg = MemoryConfig::default();
        assert!(cfg.enabled);
    }

    #[test]
    fn sandbox_config_defaults() {
        let cfg = SandboxConfig::default();
        assert!(cfg.enabled);
        assert!(matches!(cfg.backend, SandboxBackend::Auto));
    }

    // ── Validation tests ──

    #[test]
    fn default_config_validates_ok() {
        let cfg = NgenOrcaConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_catches_zero_port() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.gateway.port = 0;
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("port")));
    }

    #[test]
    fn validate_catches_empty_bind() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.gateway.bind = String::new();
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("bind")));
    }

    #[test]
    fn validate_catches_missing_auth_password() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.gateway.auth_mode = AuthMode::Password;
        cfg.gateway.auth_password = None;
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("auth_password")));
    }

    #[test]
    fn validate_catches_empty_auth_tokens() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.gateway.auth_mode = AuthMode::Token;
        cfg.gateway.auth_tokens = vec![];
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("auth_tokens")));
    }

    #[test]
    fn validate_catches_missing_tls_cert() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.gateway.auth_mode = AuthMode::Certificate;
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("tls_cert")));
        assert!(errs.iter().any(|e| e.contains("tls_key")));
    }

    #[test]
    fn validate_catches_empty_model() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.model = String::new();
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("agent.model")));
    }

    #[test]
    fn validate_catches_bad_confidence_threshold() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.classifier = Some(ClassifierConfig {
            model: "test/model".into(),
            confidence_threshold: 1.5,
            max_tokens: 100,
            temperature: 0.5,
        });
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("confidence_threshold")));
    }

    #[test]
    fn validate_catches_duplicate_sub_agent_names() {
        let mut cfg = NgenOrcaConfig::default();
        let sa = SubAgentConfig {
            name: "dup".into(),
            model: "ollama/llama3".into(),
            roles: vec!["general".into()],
            system_prompt: None,
            max_tokens: 2048,
            temperature: 0.3,
            max_complexity: "Moderate".into(),
            is_local: true,
            cost_weight: 1,
            priority: 10,
        };
        cfg.agent.sub_agents = vec![sa.clone(), sa];
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("duplicate sub_agent")));
    }

    #[test]
    fn validate_warns_zero_consolidation_interval() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.memory.consolidation_interval_secs = 0;
        let warnings = cfg.validate().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("consolidation_interval"))
        );
    }

    #[test]
    fn validate_warns_unknown_quality_gate_method() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.quality_gate.method = "unknown_method".into();
        let warnings = cfg.validate().unwrap();
        assert!(warnings.iter().any(|w| w.contains("quality_gate.method")));
    }

    #[test]
    fn validate_catches_rate_limit_zero_window() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.gateway.rate_limit_max = 100;
        cfg.gateway.rate_limit_window_secs = 0;
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("rate_limit_window_secs")));
    }

    #[test]
    fn validate_whatsapp_native_mode_passes() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.channels.whatsapp = Some(WhatsAppChannelConfig {
            enabled: true,
            phone_number_id: None,
            access_token: None,
            verify_token: None,
            webhook_path: "/webhooks/whatsapp".into(),
            app_secret: None,
            data_path: None,
        });
        // Native mode (no access_token) should pass validation.
        let result = cfg.validate();
        if let Err(errs) = result {
            assert!(!errs.iter().any(|e| e.contains("whatsapp")));
        }
    }

    #[test]
    fn validate_catches_missing_slack_token() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.channels.slack = Some(SlackChannelConfig {
            enabled: true,
            bot_token: None,
            app_token: None,
            signing_secret: None,
            socket_mode: true,
            channel_ids: vec![],
        });
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("slack.bot_token")));
    }

    #[test]
    fn validate_catches_missing_signal_phone() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.channels.signal = Some(SignalChannelConfig {
            enabled: true,
            phone_number: None,
            signal_cli_path: None,
            data_path: None,
            mode: "daemon".into(),
        });
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("signal.phone_number")));
    }

    #[test]
    fn validate_catches_missing_matrix_token() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.channels.matrix = Some(MatrixChannelConfig {
            enabled: true,
            homeserver: None,
            user_id: None,
            access_token: None,
            device_id: None,
            auto_join: false,
            encrypted: false,
        });
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("matrix.access_token")));
    }

    #[test]
    fn validate_catches_missing_teams_app_id() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.channels.teams = Some(TeamsChannelConfig {
            enabled: true,
            app_id: None,
            app_password: None,
            tenant_id: "common".into(),
            webhook_url: None,
        });
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("teams.app_id")));
    }

    #[test]
    fn validate_catches_bad_provider_url() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.providers.anthropic = Some(AnthropicProviderConfig {
            api_key: Some("key".into()),
            base_url: "not-a-url".into(),
            api_version: "2023-06-01".into(),
            max_tokens: None,
            temperature: None,
        });
        let errs = cfg.validate().unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("base_url") && e.contains("http"))
        );
    }

    #[test]
    fn validate_catches_bad_temperature() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.providers.anthropic = Some(AnthropicProviderConfig {
            api_key: Some("key".into()),
            base_url: "https://api.anthropic.com".into(),
            api_version: "2023-06-01".into(),
            max_tokens: None,
            temperature: Some(3.0),
        });
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("temperature")));
    }

    #[test]
    fn validate_catches_zero_max_tokens() {
        let mut cfg = NgenOrcaConfig::default();
        cfg.agent.providers.openai = Some(OpenAIProviderConfig {
            api_key: Some("key".into()),
            base_url: "https://api.openai.com/v1".into(),
            organization: None,
            max_tokens: Some(0),
            temperature: None,
        });
        let errs = cfg.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("max_tokens")));
    }
}
