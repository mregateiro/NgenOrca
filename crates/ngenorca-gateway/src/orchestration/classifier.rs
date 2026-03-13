//! Rule-based task classifier (Level 1 — zero cost).
//!
//! Uses regex patterns and keyword matching to classify user messages.
//! Falls through to SLM/LLM classifiers when confidence is low.

use async_trait::async_trait;
use ngenorca_core::Result;
use ngenorca_core::orchestration::{
    ClassificationMethod, TaskClassification, TaskComplexity, TaskIntent,
};
use ngenorca_plugin_sdk::{ChatMessage, TaskClassifier};
use tracing::debug;

/// A zero-cost, regex/keyword-based task classifier.
pub struct RuleBasedClassifier {
    /// Minimum message length to consider "complex".
    complex_threshold: usize,
}

impl RuleBasedClassifier {
    pub fn new() -> Self {
        Self {
            complex_threshold: 500,
        }
    }
}

impl Default for RuleBasedClassifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Keyword patterns for intent detection.
struct IntentPattern {
    intent: TaskIntent,
    keywords: &'static [&'static str],
    /// Base complexity for this intent.
    base_complexity: TaskComplexity,
    /// Confidence when matched.
    confidence: f64,
}

const PATTERNS: &[IntentPattern] = &[
    IntentPattern {
        intent: TaskIntent::Summarization,
        keywords: &[
            "resum", "summar", "sintetiz", "tldr", "tl;dr", "condensa", "shorten", "brief",
            "digest",
        ],
        base_complexity: TaskComplexity::Simple,
        confidence: 0.85,
    },
    IntentPattern {
        intent: TaskIntent::Translation,
        keywords: &[
            "traduz",
            "translat",
            "traduc",
            "converti",
            "em inglês",
            "em português",
            "in english",
            "in portuguese",
            "en español",
            "übersetze",
            "traduire",
        ],
        base_complexity: TaskComplexity::Simple,
        confidence: 0.90,
    },
    IntentPattern {
        intent: TaskIntent::Coding,
        keywords: &[
            "código",
            "code",
            "function",
            "função",
            "class",
            "struct",
            "impl ",
            "debug",
            "fix this",
            "corrige",
            "refactor",
            "compile",
            "error:",
            "panic",
            "bug",
            "programa",
            "script",
            "api",
            "endpoint",
            "query",
            "sql",
            "html",
            "css",
            "python",
            "rust",
            "javascript",
            "typescript",
        ],
        base_complexity: TaskComplexity::Moderate,
        confidence: 0.80,
    },
    IntentPattern {
        intent: TaskIntent::Analysis,
        keywords: &[
            "analis",
            "analyz",
            "compar",
            "evaluat",
            "avali",
            "diagnos",
            "investig",
            "explain why",
            "explica porquê",
            "what went wrong",
            "root cause",
        ],
        base_complexity: TaskComplexity::Complex,
        confidence: 0.75,
    },
    IntentPattern {
        intent: TaskIntent::Creative,
        keywords: &[
            "escreve",
            "write",
            "cria",
            "creat",
            "story",
            "história",
            "poem",
            "poema",
            "email",
            "carta",
            "letter",
            "brainstorm",
            "ideia",
            "idea",
            "slogan",
            "tagline",
            "name for",
        ],
        base_complexity: TaskComplexity::Moderate,
        confidence: 0.80,
    },
    IntentPattern {
        intent: TaskIntent::Extraction,
        keywords: &[
            "extrai",
            "extract",
            "parse",
            "json from",
            "tabela",
            "table from",
            "list all",
            "find all",
            "pega em",
            "structured",
        ],
        base_complexity: TaskComplexity::Simple,
        confidence: 0.80,
    },
    IntentPattern {
        intent: TaskIntent::Reasoning,
        keywords: &[
            "calcula",
            "calculat",
            "math",
            "equação",
            "equation",
            "solve",
            "resolve",
            "proof",
            "prova",
            "logic",
            "lógica",
        ],
        base_complexity: TaskComplexity::Moderate,
        confidence: 0.85,
    },
    IntentPattern {
        intent: TaskIntent::Planning,
        keywords: &[
            "planeia",
            "plan ",
            "schedul",
            "organiz",
            "roadmap",
            "timeline",
            "step by step",
            "passo a passo",
            "how to",
            "como",
            "strategy",
            "estratégia",
        ],
        base_complexity: TaskComplexity::Complex,
        confidence: 0.75,
    },
    IntentPattern {
        intent: TaskIntent::QuestionAnswering,
        keywords: &[
            "o que é", "what is", "who is", "quem é", "when did", "quando", "where is", "onde",
            "how many", "quantos", "define", "explain", "explica",
        ],
        base_complexity: TaskComplexity::Trivial,
        confidence: 0.70,
    },
];

#[async_trait]
impl TaskClassifier for RuleBasedClassifier {
    async fn classify(
        &self,
        message: &str,
        _conversation_context: Option<&[ChatMessage]>,
    ) -> Result<TaskClassification> {
        let lower = message.to_lowercase();

        // Try each pattern
        let mut best_match: Option<(&IntentPattern, f64)> = None;

        for pattern in PATTERNS {
            let matched_count = pattern
                .keywords
                .iter()
                .filter(|kw| lower.contains(*kw))
                .count();

            if matched_count > 0 {
                // Boost confidence with more keyword matches
                let confidence =
                    (pattern.confidence + (matched_count as f64 - 1.0) * 0.05).min(0.95);

                if best_match.as_ref().is_none_or(|(_, c)| confidence > *c) {
                    best_match = Some((pattern, confidence));
                }
            }
        }

        let (intent, complexity, confidence) = if let Some((pattern, conf)) = best_match {
            // Adjust complexity based on message length
            let complexity = if message.len() > self.complex_threshold {
                match pattern.base_complexity {
                    TaskComplexity::Trivial => TaskComplexity::Simple,
                    TaskComplexity::Simple => TaskComplexity::Moderate,
                    c => c,
                }
            } else {
                pattern.base_complexity
            };

            (pattern.intent.clone(), complexity, conf)
        } else {
            // No match — return Unknown with low confidence
            // The orchestrator will escalate to SLM/LLM classifier
            let complexity = if message.len() > self.complex_threshold {
                TaskComplexity::Moderate
            } else {
                TaskComplexity::Simple
            };
            (TaskIntent::Unknown, complexity, 0.3)
        };

        debug!(
            ?intent,
            ?complexity,
            confidence,
            method = "rule_based",
            msg_len = message.len(),
            "Task classified"
        );

        Ok(TaskClassification {
            intent,
            complexity,
            confidence,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: detect_language(&lower),
        })
    }

    fn classifier_name(&self) -> &str {
        "rule-based"
    }
}

/// Simple language detection based on common words.
fn detect_language(text: &str) -> Option<String> {
    let pt_markers = [
        "é", "não", "está", "como", "para", "mais", "pode", "tem", "são", "uma",
    ];
    let en_markers = [
        "the", "is", "are", "this", "that", "with", "for", "and", "not", "can",
    ];
    let es_markers = [
        "está", "pero", "también", "puede", "tiene", "esto", "como", "más", "por",
    ];

    let pt_score: usize = pt_markers.iter().filter(|w| text.contains(*w)).count();
    let en_score: usize = en_markers.iter().filter(|w| text.contains(*w)).count();
    let es_score: usize = es_markers.iter().filter(|w| text.contains(*w)).count();

    let max = pt_score.max(en_score).max(es_score);
    if max < 2 {
        return None;
    }

    if pt_score >= en_score && pt_score >= es_score {
        Some("pt".into())
    } else if es_score >= en_score {
        Some("es".into())
    } else {
        Some("en".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_summarization_detection() {
        let c = RuleBasedClassifier::new();
        let r = c
            .classify("resume este artigo sobre redes", None)
            .await
            .unwrap();
        assert_eq!(r.intent, TaskIntent::Summarization);
        assert!(r.confidence > 0.8);
    }

    #[tokio::test]
    async fn test_coding_detection() {
        let c = RuleBasedClassifier::new();
        let r = c
            .classify("write a function to sort a list in rust", None)
            .await
            .unwrap();
        assert_eq!(r.intent, TaskIntent::Coding);
    }

    #[tokio::test]
    async fn test_unknown_fallback() {
        let c = RuleBasedClassifier::new();
        let r = c.classify("olá, tudo bem?", None).await.unwrap();
        // Casual greetings don't match specific intents
        assert!(r.confidence < 0.5);
    }

    #[tokio::test]
    async fn test_translation_detection() {
        let c = RuleBasedClassifier::new();
        let r = c.classify("traduz isto para inglês", None).await.unwrap();
        assert_eq!(r.intent, TaskIntent::Translation);
        assert!(r.confidence > 0.85);
    }

    #[tokio::test]
    async fn test_language_detection_pt() {
        let lang = detect_language("como é que posso fazer isto para funcionar mais rápido");
        assert_eq!(lang, Some("pt".into()));
    }
}
