//! # NgenOrca Plugin SDK
//!
//! Every channel adapter, tool, model provider, and extension implements
//! the `Plugin` trait. The gateway loads plugins, checks their permissions,
//! manages their lifecycle, and routes events to them.
//!
//! ## Plugin kinds:
//! - **ChannelAdapter**: bridges an external messaging platform
//! - **Tool**: gives the agent a capability (browser, file, exec, etc.)
//! - **ModelProvider**: wraps an LLM API (OpenAI, Ollama, etc.)
//! - **Extension**: arbitrary hook into the gateway lifecycle
//!
//! ## Plugin lifecycle:
//! 1. `manifest()` — declare name, version, kind, permissions
//! 2. `init()` — called once when loaded; receive a `PluginContext`
//! 3. `health_check()` — periodic health probe
//! 4. `shutdown()` — graceful teardown

use async_trait::async_trait;
use ngenorca_core::Result;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{API_VERSION, PluginManifest};

/// Context provided to plugins by the gateway.
pub struct PluginContext {
    /// Send a message back into the bus.
    pub sender: flume_like::Sender,
    /// Plugin's own config fragment (deserialized from JSON).
    pub config: serde_json::Value,
    /// Data directory for this plugin.
    pub data_dir: std::path::PathBuf,
}

/// Simplified sender interface (plugins use this to send messages back).
pub mod flume_like {
    use ngenorca_core::event::Event;

    /// Channel sender that plugins use to emit events.
    #[derive(Clone)]
    pub struct Sender {
        tx: tokio::sync::mpsc::UnboundedSender<Event>,
    }

    impl Sender {
        pub fn new(tx: tokio::sync::mpsc::UnboundedSender<Event>) -> Self {
            Self { tx }
        }

        pub fn send(&self, event: Event) -> ngenorca_core::Result<()> {
            self.tx
                .send(event)
                .map_err(|_| ngenorca_core::Error::ChannelClosed)
        }
    }
}

/// The core plugin trait. Every plugin must implement this.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Return the plugin manifest (name, version, kind, permissions).
    fn manifest(&self) -> PluginManifest;

    /// Initialize the plugin with a gateway-provided context.
    async fn init(&mut self, ctx: PluginContext) -> Result<()>;

    /// Handle an incoming message (routed by the gateway based on plugin kind).
    async fn handle_message(&self, message: &Message) -> Result<Option<Message>>;

    /// Periodic health check. Return Err if unhealthy.
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    /// Graceful shutdown.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    /// API version this plugin targets.
    fn api_version(&self) -> u32 {
        API_VERSION
    }
}

/// A tool definition that agents can discover and invoke.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDefinition {
    /// Tool name (used in tool calls).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub parameters: serde_json::Value,
    /// Whether this tool requires sandbox execution.
    pub requires_sandbox: bool,
}

/// Trait for tools that the agent can invoke.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Describe this tool (for the agent's tool list).
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given arguments.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        session_id: &ngenorca_core::SessionId,
        user_id: Option<&ngenorca_core::UserId>,
    ) -> Result<serde_json::Value>;
}

/// Trait for channel adapters.
#[async_trait]
pub trait ChannelAdapter: Plugin {
    /// Start listening for inbound messages from this channel.
    async fn start_listening(&self) -> Result<()>;

    /// Send an outbound message to this channel.
    async fn send_message(&self, message: &Message) -> Result<()>;

    /// Get which channel kind this adapter handles.
    fn channel_kind(&self) -> ngenorca_core::ChannelKind;
}

/// Trait for model providers (LLM API wrappers).
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// List available models from this provider.
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;

    /// Send a chat completion request.
    async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse>;

    /// Provider name (e.g., "openai", "anthropic", "ollama").
    fn provider_name(&self) -> &str;
}

/// Info about an available model.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_window: usize,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub is_local: bool,
}

/// A chat completion request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub max_tokens: Option<usize>,
    pub temperature: Option<f64>,
}

/// A chat message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// A chat completion response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatCompletionResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallResponse>,
    pub usage: Usage,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallResponse {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

// ─── Multi-Agent Orchestration Traits ───────────────────────────

use ngenorca_core::orchestration::{
    LearnedRoutingRule, OrchestrationRecord, QualityMethod, QualityVerdict, RoutingDecision,
    SubAgentId, TaskClassification,
};

/// Trait for task classifiers — determine what kind of task a message represents.
///
/// The orchestrator uses this to decide which sub-agent should handle the request.
/// Implementations can range from simple regex matchers to SLM-based classifiers.
#[async_trait]
pub trait TaskClassifier: Send + Sync {
    /// Classify a user message into an intent + complexity.
    async fn classify(
        &self,
        message: &str,
        conversation_context: Option<&[ChatMessage]>,
    ) -> Result<TaskClassification>;

    /// Classifier name (for logging/metrics).
    fn classifier_name(&self) -> &str;
}

/// Trait for the quality gate — evaluates whether a sub-agent's response is
/// good enough or needs to be escalated/augmented.
#[async_trait]
pub trait QualityGate: Send + Sync {
    /// Evaluate a sub-agent's response against the original task.
    async fn evaluate(
        &self,
        task: &TaskClassification,
        response: &ChatCompletionResponse,
        original_message: &str,
    ) -> Result<(QualityVerdict, QualityMethod)>;

    /// Gate name (for logging/metrics).
    fn gate_name(&self) -> &str;
}

/// Trait for the agent orchestrator — the brain that routes tasks to sub-agents,
/// generates dynamic prompts, and manages the classify → route → delegate →
/// evaluate cycle.
#[async_trait]
pub trait AgentOrchestrator: Send + Sync {
    /// Process a user message through the full orchestration pipeline:
    /// classify → route → delegate → quality-check → respond (or escalate).
    async fn process(
        &self,
        message: &str,
        conversation: &[ChatMessage],
        session_id: &ngenorca_core::SessionId,
        user_id: Option<&ngenorca_core::UserId>,
    ) -> Result<OrchestratedResponse>;

    /// Get available sub-agents and their roles.
    fn available_agents(&self) -> Vec<SubAgentId>;

    /// Get learned routing rules from Tier-3 memory.
    async fn learned_rules(&self) -> Result<Vec<LearnedRoutingRule>>;

    /// Record an orchestration result (for learning).
    async fn record_result(&self, record: OrchestrationRecord) -> Result<()>;
}

/// The result of a full orchestration cycle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrchestratedResponse {
    /// The final response content.
    pub content: String,
    /// Tool calls (if any).
    pub tool_calls: Vec<ToolCallResponse>,
    /// Which sub-agent produced the final response.
    pub served_by: SubAgentId,
    /// The task classification.
    pub classification: TaskClassification,
    /// The routing decision.
    pub routing: RoutingDecision,
    /// Quality verdict.
    pub quality: QualityVerdict,
    /// Whether escalation occurred.
    pub escalated: bool,
    /// Total tokens used across all models in the pipeline.
    pub total_usage: Usage,
    /// Total latency in milliseconds.
    pub latency_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::event::{Event, EventPayload, LifecycleEvent};
    use ngenorca_core::orchestration::*;
    use ngenorca_core::types::*;

    // ─── ToolDefinition tests ───

    #[test]
    fn tool_definition_serde_roundtrip() {
        let td = ToolDefinition {
            name: "web_search".into(),
            description: "Search the web".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            requires_sandbox: false,
        };
        let json = serde_json::to_string(&td).unwrap();
        let back: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "web_search");
        assert!(!back.requires_sandbox);
    }

    // ─── ModelInfo tests ───

    #[test]
    fn model_info_serde_roundtrip() {
        let mi = ModelInfo {
            id: "gpt-4".into(),
            name: "GPT-4".into(),
            context_window: 128_000,
            supports_tools: true,
            supports_vision: true,
            is_local: false,
        };
        let json = serde_json::to_string(&mi).unwrap();
        let back: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "gpt-4");
        assert_eq!(back.context_window, 128_000);
        assert!(back.supports_tools);
        assert!(back.supports_vision);
        assert!(!back.is_local);
    }

    #[test]
    fn model_info_local_model() {
        let mi = ModelInfo {
            id: "llama3".into(),
            name: "Llama 3".into(),
            context_window: 8192,
            supports_tools: false,
            supports_vision: false,
            is_local: true,
        };
        assert!(mi.is_local);
        assert!(!mi.supports_tools);
    }

    // ─── ChatMessage tests ───

    #[test]
    fn chat_message_serde_roundtrip() {
        let msg = ChatMessage {
            role: "user".into(),
            content: "Hello!".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, "user");
        assert_eq!(back.content, "Hello!");
    }

    // ─── ChatCompletionRequest tests ───

    #[test]
    fn chat_completion_request_serde_roundtrip() {
        let req = ChatCompletionRequest {
            model: "gpt-4".into(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: "You are helpful.".into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content: "Hi".into(),
                },
            ],
            tools: None,
            max_tokens: Some(1024),
            temperature: Some(0.7),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ChatCompletionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.model, "gpt-4");
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.max_tokens, Some(1024));
        assert!((back.temperature.unwrap() - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn chat_completion_request_with_tools() {
        let tool = ToolDefinition {
            name: "calc".into(),
            description: "Calculator".into(),
            parameters: serde_json::json!({}),
            requires_sandbox: false,
        };
        let req = ChatCompletionRequest {
            model: "gpt-4".into(),
            messages: vec![],
            tools: Some(vec![tool]),
            max_tokens: None,
            temperature: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ChatCompletionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tools.unwrap().len(), 1);
    }

    // ─── ChatCompletionResponse tests ───

    #[test]
    fn chat_completion_response_serde_roundtrip() {
        let resp = ChatCompletionResponse {
            content: Some("Hello!".into()),
            tool_calls: vec![],
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ChatCompletionResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content.unwrap(), "Hello!");
        assert!(back.tool_calls.is_empty());
        assert_eq!(back.usage.total_tokens, 15);
    }

    #[test]
    fn chat_completion_response_with_tool_calls() {
        let resp = ChatCompletionResponse {
            content: None,
            tool_calls: vec![ToolCallResponse {
                id: "call_1".into(),
                name: "web_search".into(),
                arguments: serde_json::json!({"query": "rust lang"}),
            }],
            usage: Usage::default(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ChatCompletionResponse = serde_json::from_str(&json).unwrap();
        assert!(back.content.is_none());
        assert_eq!(back.tool_calls.len(), 1);
        assert_eq!(back.tool_calls[0].name, "web_search");
    }

    // ─── ToolCallResponse tests ───

    #[test]
    fn tool_call_response_serde_roundtrip() {
        let tc = ToolCallResponse {
            id: "call_abc".into(),
            name: "execute_code".into(),
            arguments: serde_json::json!({"code": "print('hi')"}),
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: ToolCallResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "call_abc");
        assert_eq!(back.name, "execute_code");
    }

    // ─── Usage tests ───

    #[test]
    fn usage_default_is_zero() {
        let u = Usage::default();
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.total_tokens, 0);
    }

    #[test]
    fn usage_serde_roundtrip() {
        let u = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        let json = serde_json::to_string(&u).unwrap();
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.prompt_tokens, 100);
        assert_eq!(back.completion_tokens, 50);
        assert_eq!(back.total_tokens, 150);
    }

    // ─── Helper to build SubAgentId ───

    fn agent_id(name: &str, model: &str) -> SubAgentId {
        SubAgentId {
            name: name.into(),
            model: model.into(),
        }
    }

    fn sample_classification() -> TaskClassification {
        TaskClassification {
            intent: TaskIntent::QuestionAnswering,
            complexity: TaskComplexity::Simple,
            confidence: 0.95,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: None,
        }
    }

    fn sample_routing() -> RoutingDecision {
        RoutingDecision {
            target: agent_id("agent-fast", "ollama/llama3"),
            reason: "Simple QA task".into(),
            system_prompt: "You are a helpful assistant.".into(),
            temperature: None,
            max_tokens: None,
            from_memory: false,
        }
    }

    // ─── OrchestratedResponse tests ───

    #[test]
    fn orchestrated_response_serde_roundtrip() {
        let resp = OrchestratedResponse {
            content: "Here is the answer.".into(),
            tool_calls: vec![],
            served_by: agent_id("agent-fast", "ollama/llama3"),
            classification: sample_classification(),
            routing: sample_routing(),
            quality: QualityVerdict::Accept { score: Some(0.9) },
            escalated: false,
            total_usage: Usage {
                prompt_tokens: 50,
                completion_tokens: 20,
                total_tokens: 70,
            },
            latency_ms: 150,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: OrchestratedResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content, "Here is the answer.");
        assert_eq!(back.served_by.name, "agent-fast");
        assert!(!back.escalated);
        assert_eq!(back.latency_ms, 150);
    }

    #[test]
    fn orchestrated_response_escalated() {
        let resp = OrchestratedResponse {
            content: "Escalated result.".into(),
            tool_calls: vec![],
            served_by: agent_id("agent-heavy", "openai/gpt-4"),
            classification: TaskClassification {
                intent: TaskIntent::Coding,
                complexity: TaskComplexity::Complex,
                confidence: 0.7,
                method: ClassificationMethod::SlmClassifier,
                domain_tags: vec!["rust".into()],
                language: Some("en".into()),
            },
            routing: RoutingDecision {
                target: agent_id("agent-fast", "ollama/llama3"),
                reason: "Initial attempt".into(),
                system_prompt: "Code assistant".into(),
                temperature: Some(0.2),
                max_tokens: Some(4096),
                from_memory: false,
            },
            quality: QualityVerdict::Escalate {
                reason: "Code quality insufficient".into(),
                escalate_to: Some("openai/gpt-4".into()),
            },
            escalated: true,
            total_usage: Usage::default(),
            latency_ms: 3000,
        };
        assert!(resp.escalated);
        assert_eq!(resp.latency_ms, 3000);
    }

    // ─── flume_like::Sender tests ───

    #[tokio::test]
    async fn sender_send_success() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = flume_like::Sender::new(tx);
        let event = Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: None,
            payload: EventPayload::SystemLifecycle(LifecycleEvent::GatewayStarted),
        };
        sender.send(event).unwrap();
        let received = rx.recv().await.unwrap();
        assert!(matches!(
            received.payload,
            EventPayload::SystemLifecycle(LifecycleEvent::GatewayStarted)
        ));
    }

    #[tokio::test]
    async fn sender_send_closed_channel_errors() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let sender = flume_like::Sender::new(tx);
        drop(rx); // close receiver
        let event = Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: None,
            user_id: None,
            payload: EventPayload::SystemLifecycle(LifecycleEvent::GatewayShutdown),
        };
        assert!(sender.send(event).is_err());
    }

    // ─── PluginContext tests ───

    #[test]
    fn plugin_context_construction() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = PluginContext {
            sender: flume_like::Sender::new(tx),
            config: serde_json::json!({"key": "value"}),
            data_dir: std::path::PathBuf::from("/tmp/plugin"),
        };
        assert_eq!(ctx.config["key"], "value");
        assert_eq!(ctx.data_dir, std::path::PathBuf::from("/tmp/plugin"));
    }
}
