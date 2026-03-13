//! Quality gate implementations.
//!
//! **Heuristic** (zero cost — no LLM calls):
//! Evaluates a sub-agent's response using simple heuristics:
//! - Minimum response length
//! - Language consistency
//! - Format validity (e.g., code blocks if coding task)
//! - Refusal detection ("I can't", "I don't know")
//!
//! **LLM** (uses a model to evaluate quality):
//! Sends the response to an LLM with a structured prompt asking it
//! to judge answer quality, returning Accept / Escalate / Augment.

use async_trait::async_trait;
use ngenorca_core::Result;
use ngenorca_core::orchestration::{
    QualityMethod, QualityVerdict, TaskClassification, TaskComplexity, TaskIntent,
};
use ngenorca_plugin_sdk::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, QualityGate,
};
use tracing::debug;

/// A zero-cost quality gate that uses heuristics to evaluate responses.
pub struct HeuristicQualityGate {
    /// Minimum response length (characters).
    min_length: usize,
    /// Maximum escalations before giving up.
    #[allow(dead_code)]
    max_escalations: u32,
    /// Auto-accept learned rules.
    #[allow(dead_code)]
    auto_accept_learned: bool,
}

impl HeuristicQualityGate {
    pub fn new(min_length: usize, max_escalations: u32, auto_accept_learned: bool) -> Self {
        Self {
            min_length,
            max_escalations,
            auto_accept_learned,
        }
    }

    pub fn from_config(config: &ngenorca_config::QualityGateConfig) -> Self {
        Self {
            min_length: config.min_response_length,
            max_escalations: config.max_escalations,
            auto_accept_learned: config.auto_accept_learned,
        }
    }
}

impl Default for HeuristicQualityGate {
    fn default() -> Self {
        Self::new(10, 2, true)
    }
}

/// Refusal phrases that indicate the model couldn't/wouldn't answer.
const REFUSAL_PHRASES: &[&str] = &[
    "i can't",
    "i cannot",
    "i don't know",
    "i'm not sure",
    "i am unable",
    "i'm unable",
    "as an ai",
    "as a language model",
    "i don't have access",
    "não consigo",
    "não sei",
    "não tenho",
    "não posso",
    "como modelo de linguagem",
    "desculpa, mas",
];

#[async_trait]
impl QualityGate for HeuristicQualityGate {
    async fn evaluate(
        &self,
        task: &TaskClassification,
        response: &ChatCompletionResponse,
        _original_message: &str,
    ) -> Result<(QualityVerdict, QualityMethod)> {
        let content = response.content.as_deref().unwrap_or("");
        let content_lower = content.to_lowercase();

        // Check 1: Empty or too short response
        if content.trim().len() < self.min_length {
            debug!(
                content_len = content.len(),
                min = self.min_length,
                "Quality gate: response too short"
            );
            return Ok((
                QualityVerdict::Escalate {
                    reason: format!(
                        "Response too short ({} chars, minimum {})",
                        content.trim().len(),
                        self.min_length
                    ),
                    escalate_to: None,
                },
                QualityMethod::Heuristic,
            ));
        }

        // Check 2: Refusal detection
        let refusal = REFUSAL_PHRASES
            .iter()
            .find(|phrase| content_lower.contains(*phrase));

        if let Some(phrase) = refusal {
            debug!(phrase, "Quality gate: refusal detected");
            return Ok((
                QualityVerdict::Escalate {
                    reason: format!("Model refused to answer (detected: '{}')", phrase),
                    escalate_to: None,
                },
                QualityMethod::Heuristic,
            ));
        }

        // Check 3: Coding tasks should contain code
        if task.intent == TaskIntent::Coding
            && task.complexity >= TaskComplexity::Simple
            && !content.contains("```")
            && !content.contains("    ") // indented code
            && content.len() > 50
        {
            debug!("Quality gate: coding task but no code block found");
            return Ok((
                QualityVerdict::Augment {
                    missing: "Expected code block/snippet in response".into(),
                    partial_response: content.to_string(),
                },
                QualityMethod::Heuristic,
            ));
        }

        // Check 4: Complexity-aware length check
        let min_for_complexity = match task.complexity {
            TaskComplexity::Trivial => 5,
            TaskComplexity::Simple => 20,
            TaskComplexity::Moderate => 50,
            TaskComplexity::Complex => 100,
            TaskComplexity::Expert => 200,
        };

        if content.trim().len() < min_for_complexity {
            debug!(
                content_len = content.len(),
                required = min_for_complexity,
                complexity = ?task.complexity,
                "Quality gate: response too short for complexity level"
            );
            return Ok((
                QualityVerdict::Augment {
                    missing: format!(
                        "Response seems too brief for a {:?} task ({} chars, expected ≥{})",
                        task.complexity,
                        content.trim().len(),
                        min_for_complexity,
                    ),
                    partial_response: content.to_string(),
                },
                QualityMethod::Heuristic,
            ));
        }

        // Check 5: Score based on response quality signals
        let score = compute_quality_score(content, task);

        debug!(
            score,
            intent = ?task.intent,
            content_len = content.len(),
            "Quality gate: accepted"
        );

        Ok((
            QualityVerdict::Accept { score: Some(score) },
            QualityMethod::Heuristic,
        ))
    }

    fn gate_name(&self) -> &str {
        "heuristic"
    }
}

// ─── LLM-based Quality Gate ────────────────────────────────────

/// Quality gate that uses an LLM to evaluate response quality.
///
/// Sends the original message, the response, and a structured evaluation
/// prompt to an LLM, then interprets its output to produce a verdict.
pub struct LlmQualityGate {
    /// Model to use for evaluation (e.g. the classifier model or a cheap one).
    pub model: String,
}

impl LlmQualityGate {
    pub fn new(model: String) -> Self {
        Self { model }
    }

    /// Build the evaluation prompt.
    fn build_eval_prompt(
        task: &TaskClassification,
        original_message: &str,
        response_text: &str,
    ) -> Vec<ChatMessage> {
        let system = format!(
            "You are a response quality evaluator. Given a user's question (intent: {:?}, \
             complexity: {:?}) and an assistant's response, output EXACTLY one of:\n\
             - ACCEPT <score 0.0-1.0>   — if the response is adequate\n\
             - ESCALATE <reason>        — if the response is poor and should be retried by a better model\n\
             - AUGMENT <missing topics> — if the response is partial and should be expanded\n\n\
             Respond with only the verdict line, nothing else.",
            task.intent, task.complexity
        );

        vec![
            ChatMessage {
                role: "system".into(),
                content: system,
            },
            ChatMessage {
                role: "user".into(),
                content: format!(
                    "## User question\n{}\n\n## Assistant response\n{}",
                    original_message, response_text
                ),
            },
        ]
    }

    /// Parse the LLM's verdict output.
    fn parse_verdict(output: &str) -> (QualityVerdict, QualityMethod) {
        let trimmed = output.trim();
        let lower = trimmed.to_lowercase();

        if lower.starts_with("accept") {
            // Try to parse score
            let score = trimmed
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<f64>().ok())
                .map(|s| s.clamp(0.0, 1.0));
            (
                QualityVerdict::Accept { score },
                QualityMethod::LlmEvaluator,
            )
        } else if lower.starts_with("escalate") {
            let reason = trimmed
                .get("escalate".len()..)
                .unwrap_or("Quality insufficient")
                .trim()
                .to_string();
            (
                QualityVerdict::Escalate {
                    reason,
                    escalate_to: None,
                },
                QualityMethod::LlmEvaluator,
            )
        } else if lower.starts_with("augment") {
            let missing = trimmed
                .get("augment".len()..)
                .unwrap_or("incomplete response")
                .trim()
                .to_string();
            (
                QualityVerdict::Augment {
                    missing,
                    partial_response: String::new(), // caller fills this in
                },
                QualityMethod::LlmEvaluator,
            )
        } else {
            // Couldn't parse — accept with low score rather than waste tokens
            debug!(
                output = trimmed,
                "LLM quality gate: could not parse verdict, defaulting to accept"
            );
            (
                QualityVerdict::Accept { score: Some(0.5) },
                QualityMethod::LlmEvaluator,
            )
        }
    }

    /// Evaluate using an LLM call (requires ProviderRegistry).
    pub async fn evaluate_with_provider(
        &self,
        task: &TaskClassification,
        response: &ChatCompletionResponse,
        original_message: &str,
        registry: &crate::providers::ProviderRegistry,
    ) -> Result<(QualityVerdict, QualityMethod)> {
        let response_text = response.content.as_deref().unwrap_or("");
        let messages = Self::build_eval_prompt(task, original_message, response_text);

        let eval_request = ChatCompletionRequest {
            model: self.model.clone(),
            messages,
            tools: None,
            max_tokens: Some(50),
            temperature: Some(0.0),
        };

        match registry.chat_completion(eval_request).await {
            Ok(eval_resp) => {
                let output = eval_resp.content.unwrap_or_default();
                let (mut verdict, method) = Self::parse_verdict(&output);

                // If the verdict is Augment, fill in the partial response
                if let QualityVerdict::Augment {
                    ref mut partial_response,
                    ..
                } = verdict
                {
                    *partial_response = response_text.to_string();
                }

                debug!(
                    verdict = ?verdict,
                    "LLM quality gate verdict"
                );

                Ok((verdict, method))
            }
            Err(e) => {
                // If the eval call fails, fall back to accepting
                debug!(error = %e, "LLM quality gate call failed, defaulting to accept");
                Ok((
                    QualityVerdict::Accept { score: Some(0.5) },
                    QualityMethod::Heuristic,
                ))
            }
        }
    }
}

/// Compute a quality score (0.0–1.0) based on various signals.
fn compute_quality_score(content: &str, task: &TaskClassification) -> f64 {
    let mut score = 0.5;

    // Length bonus (longer = usually more thorough, up to a point)
    let length_factor = (content.len() as f64 / 500.0).min(1.0);
    score += length_factor * 0.2;

    // Structure bonus: headings, lists, code blocks
    if content.contains('#') || content.contains("- ") || content.contains("* ") {
        score += 0.1;
    }
    if content.contains("```") {
        score += 0.1;
    }

    // Code tasks: bonus for having code
    if task.intent == TaskIntent::Coding && content.contains("```") {
        score += 0.1;
    }

    // Penalty for very repetitive content
    let words: Vec<&str> = content.split_whitespace().collect();
    if words.len() > 10 {
        let unique: std::collections::HashSet<&str> = words.iter().copied().collect();
        let diversity = unique.len() as f64 / words.len() as f64;
        if diversity < 0.3 {
            score -= 0.2; // Very repetitive
        }
    }

    score.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::orchestration::ClassificationMethod;
    use ngenorca_plugin_sdk::Usage;

    fn make_task(intent: TaskIntent, complexity: TaskComplexity) -> TaskClassification {
        TaskClassification {
            intent,
            complexity,
            confidence: 0.9,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: None,
        }
    }

    fn make_response(content: &str) -> ChatCompletionResponse {
        ChatCompletionResponse {
            content: Some(content.to_string()),
            tool_calls: vec![],
            usage: Usage::default(),
        }
    }

    #[tokio::test]
    async fn test_accept_good_response() {
        let gate = HeuristicQualityGate::default();
        let task = make_task(TaskIntent::Summarization, TaskComplexity::Simple);
        let resp = make_response(
            "This article discusses the key principles of network security, including encryption, firewalls, and access control.",
        );

        let (verdict, _) = gate
            .evaluate(&task, &resp, "resume este artigo")
            .await
            .unwrap();
        assert!(matches!(verdict, QualityVerdict::Accept { .. }));
    }

    #[tokio::test]
    async fn test_reject_too_short() {
        let gate = HeuristicQualityGate::new(20, 2, true);
        let task = make_task(TaskIntent::Analysis, TaskComplexity::Complex);
        let resp = make_response("OK");

        let (verdict, _) = gate.evaluate(&task, &resp, "analisa isto").await.unwrap();
        assert!(matches!(verdict, QualityVerdict::Escalate { .. }));
    }

    #[tokio::test]
    async fn test_detect_refusal() {
        let gate = HeuristicQualityGate::default();
        let task = make_task(TaskIntent::Coding, TaskComplexity::Moderate);
        let resp = make_response(
            "I'm sorry, but as an AI language model, I cannot write code that accesses the filesystem.",
        );

        let (verdict, _) = gate
            .evaluate(&task, &resp, "write a file reader")
            .await
            .unwrap();
        assert!(matches!(verdict, QualityVerdict::Escalate { .. }));
    }

    // ─── LLM Quality Gate parse tests ───

    #[test]
    fn parse_accept_with_score() {
        let (v, m) = LlmQualityGate::parse_verdict("ACCEPT 0.85");
        assert!(matches!(v, QualityVerdict::Accept { score: Some(s) } if (s - 0.85).abs() < 0.01));
        assert_eq!(m, QualityMethod::LlmEvaluator);
    }

    #[test]
    fn parse_accept_without_score() {
        let (v, _) = LlmQualityGate::parse_verdict("ACCEPT");
        assert!(matches!(v, QualityVerdict::Accept { score: None }));
    }

    #[test]
    fn parse_escalate() {
        let (v, _) = LlmQualityGate::parse_verdict("ESCALATE response is factually incorrect");
        assert!(
            matches!(v, QualityVerdict::Escalate { reason, .. } if reason.contains("factually"))
        );
    }

    #[test]
    fn parse_augment() {
        let (v, _) = LlmQualityGate::parse_verdict("AUGMENT missing error handling discussion");
        assert!(
            matches!(v, QualityVerdict::Augment { missing, .. } if missing.contains("error handling"))
        );
    }

    #[test]
    fn parse_unknown_defaults_to_accept() {
        let (v, _) = LlmQualityGate::parse_verdict("This is not a valid verdict.");
        assert!(matches!(v, QualityVerdict::Accept { score: Some(0.5) }));
    }
}
