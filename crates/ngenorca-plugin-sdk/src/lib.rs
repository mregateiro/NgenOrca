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
    /// Operator-facing execution diagnostics for worker and correction paths.
    pub diagnostics: OrchestrationDiagnostics,
}

/// Operator-facing execution diagnostics captured during orchestration.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OrchestrationDiagnostics {
    /// Structured worker plan, when the task was decomposed before execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<DelegationPlanDiagnostics>,
    /// Worker routing and handoff stages encountered while serving the request.
    #[serde(default)]
    pub worker_stages: Vec<WorkerExecutionTrace>,
    /// Tool verification and remediation diagnostics.
    #[serde(default)]
    pub correction: CorrectionDiagnostics,
    /// Primary synthesis diagnostics.
    #[serde(default)]
    pub synthesis: SynthesisDiagnostics,
}

/// One worker-stage execution result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerExecutionTrace {
    pub stage: String,
    pub agent: SubAgentId,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Structured execution plan emitted for complex delegated work.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DelegationPlanDiagnostics {
    pub strategy: String,
    #[serde(default)]
    pub steps: Vec<DelegationStepDiagnostics>,
}

/// One planned execution step.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DelegationStepDiagnostics {
    pub id: String,
    pub goal: String,
    pub agent: SubAgentId,
}

/// Tool correction-loop diagnostics for a single orchestration cycle.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CorrectionDiagnostics {
    pub tool_rounds: usize,
    #[serde(default)]
    pub tools_used: Vec<String>,
    pub had_failures: bool,
    pub had_blocked_calls: bool,
    pub verification_attempted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationDiagnostics>,
    pub remediation_attempted: bool,
    pub remediation_succeeded: bool,
    #[serde(default)]
    pub post_synthesis_verification_attempted: bool,
    #[serde(default)]
    pub post_synthesis_drift_corrected: bool,
    #[serde(default)]
    pub attempt_trace: Vec<CorrectionAttemptTrace>,
}

/// One tool-call correction attempt captured for operator diagnostics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorrectionAttemptTrace {
    pub round: usize,
    pub tool: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// Verification-pass diagnostics.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct VerificationDiagnostics {
    pub grounded: bool,
    pub should_retry_tools: bool,
    #[serde(default)]
    pub issues: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_instruction: Option<String>,
}

/// Primary-synthesis diagnostics.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SynthesisDiagnostics {
    pub attempted: bool,
    pub succeeded: bool,
    pub used_primary: bool,
    pub fallback_to_worker: bool,
    pub memory_slicing_applied: bool,
    #[serde(default)]
    pub contradiction_score: f64,
    #[serde(default)]
    pub conflicting_branches: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contradiction_anchor_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_strategy: Option<String>,
    #[serde(default)]
    pub conflict_summary: Vec<String>,
    #[serde(default)]
    pub contradiction_signals: Vec<String>,
    #[serde(default)]
    pub branch_evidence: Vec<BranchEvidenceDiagnostics>,
}

/// Structured evidence and memory-slice metadata captured per delegated branch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BranchEvidenceDiagnostics {
    pub stage: String,
    pub agent: SubAgentId,
    pub branch_role: String,
    pub memory_scope: String,
    pub evidence_focus: String,
    #[serde(default)]
    pub evidence_items: Vec<String>,
}

/// Review and lifecycle status for a reusable skill artifact.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillArtifactStatus {
    #[default]
    Draft,
    Reviewed,
    Approved,
    Deprecated,
}

/// Lifecycle and governance metadata for a stored skill artifact.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillLifecycle {
    #[serde(default = "default_skill_version")]
    pub version: u32,
    #[serde(default)]
    pub status: SkillArtifactStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub usage_count: u64,
    #[serde(default)]
    pub execution_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_executed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checkpoint_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_execution_status: Option<String>,
    #[serde(default = "default_requires_operator_review")]
    pub requires_operator_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default)]
    pub review_notes: Vec<String>,
}

impl Default for SkillLifecycle {
    fn default() -> Self {
        Self {
            version: default_skill_version(),
            status: SkillArtifactStatus::Draft,
            created_at: None,
            updated_at: None,
            last_used_at: None,
            usage_count: 0,
            execution_count: 0,
            last_executed_at: None,
            last_checkpoint_at: None,
            last_execution_status: None,
            requires_operator_review: default_requires_operator_review(),
            reviewed_by: None,
            review_notes: Vec::new(),
        }
    }
}

fn default_skill_version() -> u32 {
    1
}

fn default_requires_operator_review() -> bool {
    true
}

/// A reusable skill or automation artifact that the assistant can store and reuse.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillArtifact {
    /// Stable artifact name.
    pub name: String,
    /// Human-readable description of when the skill should be used.
    pub description: String,
    /// Optional task-intent labels or scenarios that this skill helps with.
    #[serde(default)]
    pub intent_tags: Vec<String>,
    /// Optional domain labels (e.g. rust, deployment, docs).
    #[serde(default)]
    pub domain_tags: Vec<String>,
    /// Preferred tools or capabilities for this skill.
    #[serde(default)]
    pub preferred_tools: Vec<String>,
    /// Constraints or safety notes that apply while executing the skill.
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Ordered execution steps for the skill.
    #[serde(default)]
    pub steps: Vec<AutomationStep>,
    /// Optional examples or usage notes.
    #[serde(default)]
    pub examples: Vec<String>,
    /// Lifecycle and review metadata for the stored skill.
    #[serde(default)]
    pub lifecycle: SkillLifecycle,
}

/// One step in a reusable skill or automation recipe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AutomationStep {
    /// Short title for the step.
    pub title: String,
    /// Instruction for what to do.
    pub instruction: String,
    /// Stable identifier for execution journals and rollback planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    /// Optional tool that this step expects to use.
    #[serde(default)]
    pub tool: Option<String>,
    /// Optional example tool arguments template for the step.
    #[serde(default)]
    pub arguments: Option<serde_json::Value>,
    /// Optional verification guidance for confirming the step succeeded.
    #[serde(default)]
    pub verification: Option<String>,
    /// Optional rollback guidance if this step changes state.
    #[serde(default)]
    pub rollback: Option<String>,
    /// Explicit checkpoints that should be recorded before or after this step.
    #[serde(default)]
    pub checkpoints: Vec<String>,
    /// Platform or runtime hints relevant for staged execution.
    #[serde(default)]
    pub platform_hints: Vec<String>,
    /// Whether the operator should confirm before this step executes.
    #[serde(default)]
    pub requires_confirmation: bool,
}

/// Lightweight listing view for a stored skill artifact.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillArtifactSummary {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub intent_tags: Vec<String>,
    #[serde(default)]
    pub domain_tags: Vec<String>,
    pub step_count: usize,
    #[serde(default = "default_skill_version")]
    pub version: u32,
    #[serde(default)]
    pub status: SkillArtifactStatus,
    #[serde(default)]
    pub usage_count: u64,
    #[serde(default = "default_requires_operator_review")]
    pub requires_operator_review: bool,
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
            diagnostics: OrchestrationDiagnostics::default(),
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
            diagnostics: OrchestrationDiagnostics::default(),
        };
        assert!(resp.escalated);
        assert_eq!(resp.latency_ms, 3000);
    }

    #[test]
    fn orchestration_diagnostics_plan_serde_roundtrip() {
        let diagnostics = OrchestrationDiagnostics {
            plan: Some(DelegationPlanDiagnostics {
                strategy: "structured-sequential".into(),
                steps: vec![
                    DelegationStepDiagnostics {
                        id: "frame-task".into(),
                        goal: "Clarify constraints first.".into(),
                        agent: agent_id("planner", "anthropic/claude-sonnet-4"),
                    },
                    DelegationStepDiagnostics {
                        id: "execute-domain-work".into(),
                        goal: "Implement the requested code change.".into(),
                        agent: agent_id("coder", "ollama/codellama:13b"),
                    },
                ],
            }),
            worker_stages: vec![],
            correction: CorrectionDiagnostics {
                post_synthesis_verification_attempted: true,
                post_synthesis_drift_corrected: true,
                attempt_trace: vec![CorrectionAttemptTrace {
                    round: 1,
                    tool: "write_file".into(),
                    outcome: "failed".into(),
                    failure_class: Some("path".into()),
                    guidance: Some("Verify the target path before retrying once.".into()),
                }],
                ..Default::default()
            },
            synthesis: SynthesisDiagnostics {
                attempted: true,
                succeeded: true,
                used_primary: true,
                fallback_to_worker: false,
                memory_slicing_applied: true,
                contradiction_score: 0.35,
                conflicting_branches: 1,
                contradiction_anchor_stage: Some("execute-domain-work".into()),
                reconciliation_strategy: Some("weighted_branch_evidence".into()),
                conflict_summary: vec![
                    "cross-check branch flagged a dependency risk against the execution draft"
                        .into(),
                ],
                contradiction_signals: vec![
                    "negated_action_overlap: dependency, update".into(),
                    "conflict_markers_present".into(),
                ],
                branch_evidence: vec![BranchEvidenceDiagnostics {
                    stage: "frame-task".into(),
                    agent: agent_id("planner", "anthropic/claude-sonnet-4"),
                    branch_role: "support".into(),
                    memory_scope: "goal-and-constraint slice".into(),
                    evidence_focus: "Prioritize goals, constraints, and recent active context."
                        .into(),
                    evidence_items: vec![
                        "semantic::goal::Migrate the deployment pipeline".into(),
                        "working::user::Need a rollout plan with rollback".into(),
                    ],
                }],
            },
        };

        let json = serde_json::to_string(&diagnostics).unwrap();
        let back: OrchestrationDiagnostics = serde_json::from_str(&json).unwrap();
        let plan = back.plan.expect("structured plan should deserialize");
        assert_eq!(plan.strategy, "structured-sequential");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].agent.name, "planner");
        assert_eq!(plan.steps[1].id, "execute-domain-work");
        assert!(back.synthesis.memory_slicing_applied);
        assert_eq!(back.correction.attempt_trace.len(), 1);
        assert!(back.correction.post_synthesis_drift_corrected);
        assert_eq!(
            back.synthesis.reconciliation_strategy.as_deref(),
            Some("weighted_branch_evidence")
        );
        assert_eq!(back.synthesis.conflicting_branches, 1);
        assert_eq!(
            back.synthesis.contradiction_anchor_stage.as_deref(),
            Some("execute-domain-work")
        );
        assert_eq!(back.synthesis.contradiction_signals.len(), 2);
        assert_eq!(back.synthesis.branch_evidence.len(), 1);
        assert_eq!(back.synthesis.branch_evidence[0].stage, "frame-task");
    }

    #[test]
    fn skill_artifact_serde_roundtrip() {
        let skill = SkillArtifact {
            name: "rust-build-fix".into(),
            description: "Repair a Rust build, rerun tests, and summarize the result.".into(),
            intent_tags: vec!["Coding".into(), "ToolUse".into()],
            domain_tags: vec!["rust".into(), "workspace".into()],
            preferred_tools: vec!["read_file".into(), "run_command".into()],
            constraints: vec!["Do not claim success until tests pass.".into()],
            steps: vec![AutomationStep {
                title: "Run tests".into(),
                instruction: "Run the relevant test or build command first.".into(),
                step_id: Some("run-tests".into()),
                tool: Some("run_command".into()),
                arguments: Some(serde_json::json!({"command": "cargo", "args": ["test"]})),
                verification: Some("Confirm exit_code is 0 before summarizing success.".into()),
                rollback: Some(
                    "No rollback is required for this read-only validation step.".into(),
                ),
                checkpoints: vec!["capture failing test output before edits".into()],
                platform_hints: vec!["windows".into(), "linux".into()],
                requires_confirmation: false,
            }],
            examples: vec!["Use when a Cargo workspace has failing tests.".into()],
            lifecycle: SkillLifecycle::default(),
        };

        let json = serde_json::to_string(&skill).unwrap();
        let back: SkillArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "rust-build-fix");
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.steps[0].step_id.as_deref(), Some("run-tests"));
        assert_eq!(
            back.steps[0].rollback.as_deref(),
            Some("No rollback is required for this read-only validation step.")
        );
        assert_eq!(
            back.steps[0].checkpoints,
            vec!["capture failing test output before edits"]
        );
        assert_eq!(back.preferred_tools[0], "read_file");
    }

    #[test]
    fn skill_artifact_summary_serde_roundtrip() {
        let summary = SkillArtifactSummary {
            name: "deploy-checklist".into(),
            description: "Deployment checklist".into(),
            intent_tags: vec!["Planning".into()],
            domain_tags: vec!["deployment".into()],
            step_count: 3,
            version: 2,
            status: SkillArtifactStatus::Reviewed,
            usage_count: 4,
            requires_operator_review: false,
        };

        let json = serde_json::to_string(&summary).unwrap();
        let back: SkillArtifactSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "deploy-checklist");
        assert_eq!(back.step_count, 3);
        assert_eq!(back.version, 2);
        assert_eq!(back.status, SkillArtifactStatus::Reviewed);
        assert_eq!(back.usage_count, 4);
        assert!(!back.requires_operator_review);
    }

    #[test]
    fn skill_lifecycle_serde_roundtrip() {
        let lifecycle = SkillLifecycle {
            version: 3,
            status: SkillArtifactStatus::Approved,
            created_at: Some("2026-03-14T00:00:00Z".into()),
            updated_at: Some("2026-03-14T01:00:00Z".into()),
            last_used_at: Some("2026-03-14T02:00:00Z".into()),
            usage_count: 7,
            execution_count: 2,
            last_executed_at: Some("2026-03-14T02:30:00Z".into()),
            last_checkpoint_at: Some("2026-03-14T02:31:00Z".into()),
            last_execution_status: Some("completed".into()),
            requires_operator_review: false,
            reviewed_by: Some("ops-team".into()),
            review_notes: vec!["Validated on staging.".into()],
        };

        let json = serde_json::to_string(&lifecycle).unwrap();
        let back: SkillLifecycle = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 3);
        assert_eq!(back.status, SkillArtifactStatus::Approved);
        assert_eq!(back.usage_count, 7);
        assert_eq!(back.execution_count, 2);
        assert_eq!(back.last_execution_status.as_deref(), Some("completed"));
        assert_eq!(back.reviewed_by.as_deref(), Some("ops-team"));
        assert!(!back.requires_operator_review);
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
