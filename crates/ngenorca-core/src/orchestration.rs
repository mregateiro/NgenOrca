//! Multi-agent orchestration types.
//!
//! The orchestrator is the brain of NgenOrca's agent layer. Instead of routing
//! every request to a single LLM, it classifies each task and delegates to the
//! most appropriate sub-agent — which may be a lightweight local SLM, a
//! domain-specific fine-tuned model, or a full LLM in the cloud.
//!
//! # Architecture
//!
//! ```text
//! User message
//!      │
//!      ▼
//! ┌──────────────────────────┐
//! │  Task Classifier          │  (regex → SLM → LLM, cascading)
//! │  → intent + complexity    │
//! └────────────┬─────────────┘
//!              │
//!              ▼
//! ┌──────────────────────────┐
//! │  Router                   │  reads Tier-3 memory for learned rules
//! │  → picks sub-agent        │
//! └────────────┬─────────────┘
//!              │
//!    ┌─────────┼─────────┐
//!    ▼         ▼         ▼
//! ┌──────┐ ┌──────┐ ┌──────┐
//! │ SLM  │ │ SLM  │ │ LLM  │  sub-agents (each with dynamic system prompt)
//! │local │ │code  │ │cloud │
//! └──┬───┘ └──┬───┘ └──┬───┘
//!    │        │        │
//!    ▼        ▼        ▼
//! ┌──────────────────────────┐
//! │  Quality Gate             │  Accept / Escalate / Augment
//! └──────────────────────────┘
//! ```

use serde::{Deserialize, Serialize};

// ─── Task Classification ────────────────────────────────────────

/// The kind of task a user message represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskIntent {
    /// Casual conversation, greetings, small talk.
    Conversation,
    /// Summarise a document, article, or conversation.
    Summarization,
    /// Translate text between languages.
    Translation,
    /// Generate, review, or fix code.
    Coding,
    /// Analyse data, logs, or complex documents.
    Analysis,
    /// Creative writing, brainstorming.
    Creative,
    /// Simple question-answering (factual lookup).
    QuestionAnswering,
    /// Planning, scheduling, multi-step reasoning.
    Planning,
    /// Extract structured data from unstructured text.
    Extraction,
    /// Math, calculations, logic puzzles.
    Reasoning,
    /// Image/vision-related tasks.
    Vision,
    /// Tool use (file operations, web search, etc.).
    ToolUse,
    /// Unknown or ambiguous — the orchestrator must decide.
    Unknown,
    /// Custom intent (extensible via plugins).
    Custom(String),
}

/// How complex a task appears to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TaskComplexity {
    /// Trivial — one-shot response, no reasoning needed.
    Trivial,
    /// Simple — straightforward but may need some context.
    Simple,
    /// Moderate — needs multi-step reasoning or domain knowledge.
    Moderate,
    /// Complex — requires deep analysis, planning, or long output.
    Complex,
    /// Expert — multi-domain, requires tool orchestration or chaining.
    Expert,
}

/// The result of classifying a user message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskClassification {
    /// What kind of task this is.
    pub intent: TaskIntent,
    /// Estimated complexity.
    pub complexity: TaskComplexity,
    /// Confidence in this classification (0.0–1.0).
    pub confidence: f64,
    /// How the classification was made.
    pub method: ClassificationMethod,
    /// Optional domain tags (e.g., "networking", "rust", "finance").
    pub domain_tags: Vec<String>,
    /// Optional language detected.
    pub language: Option<String>,
}

/// How the classification was derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClassificationMethod {
    /// Matched by regex/keyword rules (zero cost).
    RuleBased,
    /// Classified by a lightweight SLM (~50ms).
    SlmClassifier,
    /// Classified by the orchestrator LLM (~1-2s).
    LlmClassifier,
    /// Retrieved from Tier-3 semantic memory (learned pattern).
    LearnedRule,
}

// ─── Sub-Agent ──────────────────────────────────────────────────

/// Role a sub-agent can fulfil.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubAgentRole {
    /// General-purpose tasks.
    General,
    /// Summarisation specialist.
    Summarization,
    /// Translation specialist.
    Translation,
    /// Code generation / review / debugging.
    Coding,
    /// Data analysis.
    Analysis,
    /// Creative writing.
    Creative,
    /// Factual Q&A.
    QuestionAnswering,
    /// Planning and multi-step reasoning.
    Planning,
    /// Structured data extraction.
    Extraction,
    /// Math and logic.
    Reasoning,
    /// Vision tasks.
    Vision,
    /// Custom role.
    Custom(String),
}

/// Identity of a sub-agent (used in routing decisions and logs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentId {
    /// Unique name (e.g., "local-general", "coder", "deep-thinker").
    pub name: String,
    /// Provider/model string (e.g., "ollama/llama3.1:8b").
    pub model: String,
}

// ─── Routing Decisions ──────────────────────────────────────────

/// The routing strategy the orchestrator uses to pick a sub-agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingStrategy {
    /// The orchestrator LLM decides which sub-agent to use.
    LlmRouted,
    /// Static rules based on intent → sub-agent mapping.
    RuleBased,
    /// Try local SLM first; escalate to cloud LLM if quality is insufficient.
    LocalFirst,
    /// Minimise cost: always use the cheapest model that can handle the task.
    CostOptimized,
    /// Hybrid: rules → SLM classifier → LLM (cascading, as described in design).
    Hybrid,
}

impl Default for RoutingStrategy {
    fn default() -> Self {
        Self::Hybrid
    }
}

/// A routing decision made by the orchestrator for a specific task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    /// Which sub-agent was chosen.
    pub target: SubAgentId,
    /// Why this sub-agent was chosen.
    pub reason: String,
    /// The dynamically generated system prompt for the sub-agent.
    pub system_prompt: String,
    /// Temperature override for this task.
    pub temperature: Option<f64>,
    /// Max tokens override for this task.
    pub max_tokens: Option<usize>,
    /// Whether this decision came from a learned rule.
    pub from_memory: bool,
}

// ─── Quality Gate ───────────────────────────────────────────────

/// Verdict from the quality gate after a sub-agent responds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QualityVerdict {
    /// Response is good enough — send to user.
    Accept {
        /// Optional quality score (0.0–1.0).
        score: Option<f64>,
    },
    /// Response is insufficient — escalate to a more capable model.
    Escalate {
        /// Why the response was rejected.
        reason: String,
        /// Suggested model to escalate to.
        escalate_to: Option<String>,
    },
    /// Response is partial — enrich it with a more capable model.
    Augment {
        /// What's missing from the response.
        missing: String,
        /// The partial response to build upon.
        partial_response: String,
    },
}

/// How the quality gate evaluated the response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityMethod {
    /// Simple heuristics (length, format, language check).
    Heuristic,
    /// Evaluated by a lightweight SLM.
    SlmEvaluator,
    /// Evaluated by the orchestrator LLM.
    LlmEvaluator,
    /// Auto-accepted (high-confidence routing from learned rules).
    AutoAccept,
}

// ─── Orchestration Record (for learning) ────────────────────────

/// A complete record of one orchestration cycle. Stored in Tier-2 episodic
/// memory and consolidated into Tier-3 routing rules over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationRecord {
    /// The original task classification.
    pub classification: TaskClassification,
    /// The routing decision made.
    pub routing: RoutingDecision,
    /// The quality verdict for the sub-agent's response.
    pub quality: QualityVerdict,
    /// How the quality was assessed.
    pub quality_method: QualityMethod,
    /// Whether escalation was needed.
    pub escalated: bool,
    /// Total latency in milliseconds.
    pub latency_ms: u64,
    /// Total tokens used across all models in this cycle.
    pub total_tokens: usize,
    /// Timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// ─── Learned Routing Rule (Tier-3) ──────────────────────────────

/// A routing rule distilled from repeated orchestration records.
/// Stored in Tier-3 semantic memory. Allows the system to skip
/// expensive LLM classification for known patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedRoutingRule {
    /// The intent this rule applies to.
    pub intent: TaskIntent,
    /// Optional domain filter (e.g., only "rust" coding tasks).
    pub domain_filter: Option<String>,
    /// Optional complexity threshold (e.g., only Simple/Moderate).
    pub max_complexity: Option<TaskComplexity>,
    /// The sub-agent name to route to.
    pub target_agent: String,
    /// Confidence in this rule (0.0–1.0), based on historical success rate.
    pub confidence: f64,
    /// How many successful cycles this rule is based on.
    pub sample_count: u32,
    /// When this rule was last updated.
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_complexity_ordering() {
        assert!(TaskComplexity::Trivial < TaskComplexity::Simple);
        assert!(TaskComplexity::Simple < TaskComplexity::Moderate);
        assert!(TaskComplexity::Moderate < TaskComplexity::Complex);
        assert!(TaskComplexity::Complex < TaskComplexity::Expert);
    }

    #[test]
    fn task_intent_serde_roundtrip() {
        let intents = vec![
            TaskIntent::Conversation,
            TaskIntent::Summarization,
            TaskIntent::Translation,
            TaskIntent::Coding,
            TaskIntent::Analysis,
            TaskIntent::Creative,
            TaskIntent::QuestionAnswering,
            TaskIntent::Planning,
            TaskIntent::Extraction,
            TaskIntent::Reasoning,
            TaskIntent::Vision,
            TaskIntent::ToolUse,
            TaskIntent::Unknown,
            TaskIntent::Custom("my-task".into()),
        ];
        for intent in &intents {
            let json = serde_json::to_string(intent).unwrap();
            let back: TaskIntent = serde_json::from_str(&json).unwrap();
            assert_eq!(*intent, back);
        }
    }

    #[test]
    fn task_complexity_serde_roundtrip() {
        let complexities = vec![
            TaskComplexity::Trivial,
            TaskComplexity::Simple,
            TaskComplexity::Moderate,
            TaskComplexity::Complex,
            TaskComplexity::Expert,
        ];
        for c in &complexities {
            let json = serde_json::to_string(c).unwrap();
            let back: TaskComplexity = serde_json::from_str(&json).unwrap();
            assert_eq!(*c, back);
        }
    }

    #[test]
    fn classification_method_serde_roundtrip() {
        let methods = vec![
            ClassificationMethod::RuleBased,
            ClassificationMethod::SlmClassifier,
            ClassificationMethod::LlmClassifier,
            ClassificationMethod::LearnedRule,
        ];
        for m in &methods {
            let json = serde_json::to_string(m).unwrap();
            let back: ClassificationMethod = serde_json::from_str(&json).unwrap();
            assert_eq!(*m, back);
        }
    }

    #[test]
    fn routing_strategy_default_is_hybrid() {
        assert_eq!(RoutingStrategy::default(), RoutingStrategy::Hybrid);
    }

    #[test]
    fn routing_strategy_serde_roundtrip() {
        let strategies = vec![
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
    fn sub_agent_role_serde_roundtrip() {
        let roles = vec![
            SubAgentRole::General,
            SubAgentRole::Coding,
            SubAgentRole::Custom("domain-expert".into()),
        ];
        for r in &roles {
            let json = serde_json::to_string(r).unwrap();
            let back: SubAgentRole = serde_json::from_str(&json).unwrap();
            assert_eq!(*r, back);
        }
    }

    #[test]
    fn quality_verdict_accept_serde() {
        let v = QualityVerdict::Accept { score: Some(0.95) };
        let json = serde_json::to_string(&v).unwrap();
        let back: QualityVerdict = serde_json::from_str(&json).unwrap();
        if let QualityVerdict::Accept { score } = back {
            assert_eq!(score, Some(0.95));
        } else {
            panic!("Expected Accept");
        }
    }

    #[test]
    fn quality_verdict_escalate_serde() {
        let v = QualityVerdict::Escalate {
            reason: "too short".into(),
            escalate_to: Some("gpt-4".into()),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: QualityVerdict = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, QualityVerdict::Escalate { .. }));
    }

    #[test]
    fn quality_verdict_augment_serde() {
        let v = QualityVerdict::Augment {
            missing: "details".into(),
            partial_response: "partial...".into(),
        };
        let json = serde_json::to_string(&v).unwrap();
        let back: QualityVerdict = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, QualityVerdict::Augment { .. }));
    }

    #[test]
    fn quality_method_serde_roundtrip() {
        let methods = vec![
            QualityMethod::Heuristic,
            QualityMethod::SlmEvaluator,
            QualityMethod::LlmEvaluator,
            QualityMethod::AutoAccept,
        ];
        for m in &methods {
            let json = serde_json::to_string(m).unwrap();
            let back: QualityMethod = serde_json::from_str(&json).unwrap();
            assert_eq!(*m, back);
        }
    }

    #[test]
    fn task_classification_serde_roundtrip() {
        let tc = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: TaskComplexity::Moderate,
            confidence: 0.87,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["rust".into(), "async".into()],
            language: Some("en".into()),
        };
        let json = serde_json::to_string(&tc).unwrap();
        let back: TaskClassification = serde_json::from_str(&json).unwrap();
        assert_eq!(tc.intent, back.intent);
        assert_eq!(tc.complexity, back.complexity);
        assert!((tc.confidence - back.confidence).abs() < f64::EPSILON);
        assert_eq!(tc.domain_tags, back.domain_tags);
    }

    #[test]
    fn routing_decision_serde_roundtrip() {
        let rd = RoutingDecision {
            target: SubAgentId {
                name: "coder".into(),
                model: "ollama/codellama".into(),
            },
            reason: "coding task".into(),
            system_prompt: "You are a coding assistant".into(),
            temperature: Some(0.3),
            max_tokens: Some(4096),
            from_memory: false,
        };
        let json = serde_json::to_string(&rd).unwrap();
        let back: RoutingDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(rd.target.name, back.target.name);
        assert_eq!(rd.temperature, back.temperature);
    }

    #[test]
    fn learned_routing_rule_serde_roundtrip() {
        let rule = LearnedRoutingRule {
            intent: TaskIntent::Translation,
            domain_filter: Some("pt".into()),
            max_complexity: Some(TaskComplexity::Simple),
            target_agent: "translator".into(),
            confidence: 0.92,
            sample_count: 15,
            last_updated: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&rule).unwrap();
        let back: LearnedRoutingRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule.intent, back.intent);
        assert_eq!(rule.target_agent, back.target_agent);
        assert_eq!(rule.sample_count, back.sample_count);
    }
}
