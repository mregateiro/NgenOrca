//! # NgenOrca Memory
//!
//! Three-tier memory architecture:
//!
//! **Tier 1 — Working Memory** (hot):
//! Active conversation context window. Managed per-session.
//! Includes KV-cache persistence for local models.
//!
//! **Tier 2 — Episodic Memory** (warm):
//! Full conversation logs indexed by time, channel, and topic.
//! Embedding vectors for semantic retrieval (RAG over your own history).
//!
//! **Tier 3 — Semantic Memory** (cold but persistent):
//! Distilled facts, preferences, and knowledge extracted from conversations.
//! Background consolidation merges, deduplicates, and prunes.

pub mod episodic;
pub mod semantic;
pub mod working;

use ngenorca_core::Result;
use ngenorca_core::orchestration::{TaskClassification, TaskIntent};
use ngenorca_core::types::UserId;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy)]
struct RetrievalProfile {
    semantic_fraction: usize,
    episodic_fraction: usize,
    episodic_limit: usize,
}

/// The unified memory manager that coordinates all three tiers.
pub struct MemoryManager {
    pub working: working::WorkingMemory,
    pub episodic: episodic::EpisodicMemory,
    pub semantic: semantic::SemanticMemory,
}

impl MemoryManager {
    /// Initialize the memory manager.
    pub fn new(db_path: &str) -> Result<Self> {
        let episodic = episodic::EpisodicMemory::new(&format!("{db_path}/episodic.db"))?;
        let semantic = semantic::SemanticMemory::new(&format!("{db_path}/semantic.db"))?;
        let working = working::WorkingMemory::new();

        info!("Memory manager initialized (3-tier)");

        Ok(Self {
            working,
            episodic,
            semantic,
        })
    }

    /// Build the full context for an agent prompt, combining all three tiers.
    ///
    /// This is the key function: for a given user and query, it:
    /// 1. Injects relevant semantic memory (Tier 3 — what we know about this user)
    /// 2. Retrieves relevant episodic memories (Tier 2 — past conversations)
    /// 3. Appends the current working memory (Tier 1 — this conversation)
    pub fn build_context(
        &self,
        user_id: &UserId,
        session_id: &ngenorca_core::SessionId,
        current_query: &str,
        token_budget: usize,
    ) -> Result<ContextPack> {
        let profile = RetrievalProfile {
            semantic_fraction: 4,
            episodic_fraction: 4,
            episodic_limit: 5,
        };
        self.build_context_with_profile(user_id, session_id, current_query, token_budget, profile)
    }

    /// Build context with retrieval behavior adapted to the classified task.
    pub fn build_context_for_task(
        &self,
        user_id: &UserId,
        session_id: &ngenorca_core::SessionId,
        current_query: &str,
        classification: &TaskClassification,
        token_budget: usize,
    ) -> Result<ContextPack> {
        let profile = retrieval_profile(classification);
        let query = augmented_query(current_query, classification);
        self.build_context_with_profile(user_id, session_id, &query, token_budget, profile)
    }

    fn build_context_with_profile(
        &self,
        user_id: &UserId,
        session_id: &ngenorca_core::SessionId,
        current_query: &str,
        token_budget: usize,
        profile: RetrievalProfile,
    ) -> Result<ContextPack> {
        // Tier 3: Semantic memory — compact facts about this user.
        let semantic_budget = token_budget / profile.semantic_fraction.max(1);
        let episodic_budget = token_budget / profile.episodic_fraction.max(1);
        let semantic_facts = self.semantic.retrieve_for_user(user_id, semantic_budget)?;

        // Tier 2: Episodic memory — relevant past conversations.
        let episodic_results = self
            .episodic
            .search(user_id, current_query, profile.episodic_limit, episodic_budget)?;

        // Tier 1: Working memory — current session context.
        let working_messages = self.working.get_session(session_id);

        // Estimate total tokens (rough heuristic: ~4 chars per token).
        let semantic_tokens = semantic_facts.iter().map(|f| f.fact.len()).sum::<usize>() / 4;
        let episodic_tokens = episodic_results
            .iter()
            .map(|e| e.content.len())
            .sum::<usize>()
            / 4;
        let working_tokens = working_messages
            .iter()
            .map(|m| m.content.len())
            .sum::<usize>()
            / 4;
        let total_estimated_tokens = semantic_tokens + episodic_tokens + working_tokens;

        Ok(ContextPack {
            semantic_block: semantic_facts,
            episodic_snippets: episodic_results,
            working_messages,
            total_estimated_tokens,
        })
    }

    /// Consolidate episodic memories into semantic facts for a specific user.
    ///
    /// This is the background consolidation job. It:
    /// 1. Fetches recent episodic entries (last `window` duration)
    /// 2. Extracts patterns and preferences using heuristic rules
    /// 3. Stores extracted facts as semantic memories
    /// 4. Prunes excess episodic entries to `max_episodes`
    ///
    /// Returns `(facts_created, episodes_pruned)`.
    pub fn consolidate_for_user(
        &self,
        user_id: &UserId,
        window: chrono::Duration,
        max_episodes: usize,
    ) -> Result<ConsolidationResult> {
        let since = chrono::Utc::now() - window;
        let entries = self.episodic.get_recent(user_id, since, 200)?;

        if entries.is_empty() {
            return Ok(ConsolidationResult::default());
        }

        debug!(
            user = %user_id,
            entries = entries.len(),
            "Consolidating episodic memories"
        );

        let mut facts_created = 0u32;
        let existing_facts = self.semantic.retrieve_for_user(user_id, 10_000)?;
        let mut known_facts: std::collections::HashSet<String> = existing_facts
            .iter()
            .map(|f| f.fact.to_lowercase())
            .collect();

        // Scan entries for extractable patterns
        for entry in &entries {
            let extracted = Self::extract_facts_from_text(&entry.content);
            for (category, fact_text) in extracted {
                // Skip if we already have this fact (dedup)
                let key = fact_text.to_lowercase();
                if known_facts.contains(&key) {
                    continue;
                }
                let fact = semantic::SemanticFact {
                    id: 0,
                    user_id: user_id.0.clone(),
                    category,
                    fact: fact_text,
                    confidence: 0.5, // heuristic extraction → moderate confidence
                    source_episode_ids: vec![entry.id],
                    established_at: chrono::Utc::now(),
                    last_confirmed: chrono::Utc::now(),
                    access_count: 0,
                };
                match self.semantic.store_fact(&fact) {
                    Ok(_) => {
                        facts_created += 1;
                        known_facts.insert(key);
                    }
                    Err(e) => warn!(error = %e, "Failed to store consolidated fact"),
                }
            }
        }

        // Prune excess episodic entries
        let episodes_pruned = self.episodic.prune(user_id, max_episodes).unwrap_or(0);

        // Prune very old / low-confidence semantic facts
        let facts_pruned = self.semantic.prune(user_id, 0.1, 180).unwrap_or(0);

        let result = ConsolidationResult {
            entries_scanned: entries.len() as u32,
            facts_created,
            episodes_pruned: episodes_pruned as u32,
            facts_pruned: facts_pruned as u32,
        };

        info!(
            user = %user_id,
            scanned = result.entries_scanned,
            created = result.facts_created,
            ep_pruned = result.episodes_pruned,
            "Consolidation complete"
        );

        Ok(result)
    }

    /// Extract facts from conversation text using heuristic patterns.
    fn extract_facts_from_text(text: &str) -> Vec<(semantic::FactCategory, String)> {
        let mut facts = Vec::new();

        // Normalize for pattern matching
        let lower = text.to_lowercase();

        // Preference patterns: "I prefer X", "I like X", "I don't like X"
        let pref_patterns = [
            "i prefer ",
            "i like ",
            "i love ",
            "i don't like ",
            "i dislike ",
            "i hate ",
            "my favorite ",
            "my favourite ",
        ];
        for pattern in pref_patterns {
            if let Some(rest) = lower.find(pattern).map(|pos| {
                let start = pos + pattern.len();
                let remainder = &text[start..];
                let end = remainder
                    .find(['.', ',', '!', '?', '\n'])
                    .unwrap_or(remainder.len())
                    .min(120);
                remainder[..end].trim().to_string()
            }) && rest.len() >= 3
            {
                facts.push((
                    semantic::FactCategory::Preference,
                    format!("User {pattern}{rest}"),
                ));
            }
        }

        // Technical preferences: "I use X", "I work with X", "I'm using X"
        let tech_patterns = [
            "i use ",
            "i'm using ",
            "i work with ",
            "my stack ",
            "i code in ",
            "i program in ",
        ];
        for pattern in tech_patterns {
            if let Some(rest) = lower.find(pattern).map(|pos| {
                let start = pos + pattern.len();
                let remainder = &text[start..];
                let end = remainder
                    .find(['.', ',', '!', '?', '\n'])
                    .unwrap_or(remainder.len())
                    .min(120);
                remainder[..end].trim().to_string()
            }) && rest.len() >= 2
            {
                facts.push((
                    semantic::FactCategory::TechnicalPreference,
                    format!("User {pattern}{rest}"),
                ));
            }
        }

        // Personal info: "My name is X", "I am a X", "I live in X"
        let personal_patterns = [
            "my name is ",
            "i am a ",
            "i'm a ",
            "i live in ",
            "i'm from ",
            "i work at ",
            "i work as ",
            "my job is ",
        ];
        for pattern in personal_patterns {
            if let Some(rest) = lower.find(pattern).map(|pos| {
                let start = pos + pattern.len();
                let remainder = &text[start..];
                let end = remainder
                    .find(['.', ',', '!', '?', '\n'])
                    .unwrap_or(remainder.len())
                    .min(120);
                remainder[..end].trim().to_string()
            }) && rest.len() >= 2
            {
                facts.push((
                    semantic::FactCategory::PersonalInfo,
                    format!("User {pattern}{rest}"),
                ));
            }
        }

        // Goals: "I want to X", "my goal is X", "I'm trying to X"
        let goal_patterns = [
            "i want to ",
            "my goal is ",
            "i'm trying to ",
            "i need to learn ",
            "i'm learning ",
        ];
        for pattern in goal_patterns {
            if let Some(rest) = lower.find(pattern).map(|pos| {
                let start = pos + pattern.len();
                let remainder = &text[start..];
                let end = remainder
                    .find(['.', ',', '!', '?', '\n'])
                    .unwrap_or(remainder.len())
                    .min(120);
                remainder[..end].trim().to_string()
            }) && rest.len() >= 3
            {
                facts.push((
                    semantic::FactCategory::Goal,
                    format!("User {pattern}{rest}"),
                ));
            }
        }

        facts
    }

    /// PRIV-03: Delete **all** data for a user across all memory tiers.
    ///
    /// Returns a report with deletion counts per tier.
    pub fn delete_user_data(&self, user_id: &UserId) -> Result<DataDeletionReport> {
        let episodic_deleted = self.episodic.delete_for_user(user_id)?;
        let semantic_deleted = self.semantic.delete_for_user(user_id)?;

        // Working memory is keyed by SessionId, not UserId.
        // We clear all sessions that might belong to this user.
        // In production a session-user mapping would be consulted;
        // for now we document this limitation.
        let working_note = "Working memory is session-keyed; sessions expire automatically.";

        info!(
            user = %user_id,
            episodic = episodic_deleted,
            semantic = semantic_deleted,
            "Deleted all user data (PRIV-03)"
        );

        Ok(DataDeletionReport {
            episodic_deleted,
            semantic_deleted,
            working_note: working_note.to_string(),
        })
    }
}

fn retrieval_profile(classification: &TaskClassification) -> RetrievalProfile {
    match classification.intent {
        TaskIntent::QuestionAnswering | TaskIntent::Conversation => RetrievalProfile {
            semantic_fraction: 5,
            episodic_fraction: 5,
            episodic_limit: 3,
        },
        TaskIntent::Summarization | TaskIntent::Translation | TaskIntent::Extraction => {
            RetrievalProfile {
                semantic_fraction: 5,
                episodic_fraction: 3,
                episodic_limit: 4,
            }
        }
        TaskIntent::Coding | TaskIntent::ToolUse => RetrievalProfile {
            semantic_fraction: 3,
            episodic_fraction: 3,
            episodic_limit: 6,
        },
        TaskIntent::Analysis | TaskIntent::Planning | TaskIntent::Reasoning => RetrievalProfile {
            semantic_fraction: 4,
            episodic_fraction: 2,
            episodic_limit: 8,
        },
        TaskIntent::Creative => RetrievalProfile {
            semantic_fraction: 5,
            episodic_fraction: 2,
            episodic_limit: 6,
        },
        TaskIntent::Vision | TaskIntent::Unknown | TaskIntent::Custom(_) => RetrievalProfile {
            semantic_fraction: 4,
            episodic_fraction: 4,
            episodic_limit: 5,
        },
    }
}

fn augmented_query(current_query: &str, classification: &TaskClassification) -> String {
    if classification.domain_tags.is_empty() {
        return current_query.to_string();
    }

    match classification.intent {
        TaskIntent::Coding | TaskIntent::ToolUse => format!(
            "{current_query}\nrelated domains: {}\nfocus on prior technical preferences, scripts, commands, and fixes",
            classification.domain_tags.join(", ")
        ),
        TaskIntent::Analysis | TaskIntent::Planning | TaskIntent::Reasoning => format!(
            "{current_query}\nrelated domains: {}\nfocus on prior decisions, plans, constraints, and investigations",
            classification.domain_tags.join(", ")
        ),
        _ => format!(
            "{current_query}\nrelated domains: {}",
            classification.domain_tags.join(", ")
        ),
    }
}

/// Packed context ready for injection into an LLM prompt.
#[derive(Debug, Clone)]
pub struct ContextPack {
    /// Semantic memory block (facts, preferences, user profile).
    pub semantic_block: Vec<semantic::SemanticFact>,

    /// Relevant episodic memories (past conversation snippets).
    pub episodic_snippets: Vec<episodic::EpisodicEntry>,

    /// Current session messages (working memory).
    pub working_messages: Vec<working::WorkingMessage>,

    /// Estimated total token count.
    pub total_estimated_tokens: usize,
}

/// Result of a memory consolidation run.
#[derive(Debug, Clone, Default)]
pub struct ConsolidationResult {
    /// Number of episodic entries scanned.
    pub entries_scanned: u32,
    /// Number of new semantic facts created.
    pub facts_created: u32,
    /// Number of episodic entries pruned.
    pub episodes_pruned: u32,
    /// Number of semantic facts pruned (low confidence / old).
    pub facts_pruned: u32,
}

/// Report returned by `delete_user_data` (PRIV-03).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataDeletionReport {
    /// Number of episodic entries deleted.
    pub episodic_deleted: usize,
    /// Number of semantic facts deleted.
    pub semantic_deleted: usize,
    /// Note about working memory handling.
    pub working_note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_memory() -> MemoryManager {
        let tmp = std::env::temp_dir().join(format!(
            "ngenorca_mem_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        MemoryManager::new(tmp.to_str().unwrap()).unwrap()
    }

    #[test]
    fn extract_preference() {
        let facts = MemoryManager::extract_facts_from_text("I prefer dark mode for coding.");
        assert!(!facts.is_empty());
        assert!(matches!(facts[0].0, semantic::FactCategory::Preference));
        assert!(facts[0].1.contains("dark mode for coding"));
    }

    #[test]
    fn extract_technical_preference() {
        let facts = MemoryManager::extract_facts_from_text("I use Rust and Python daily.");
        assert!(!facts.is_empty());
        assert!(matches!(
            facts[0].0,
            semantic::FactCategory::TechnicalPreference
        ));
        assert!(facts[0].1.contains("Rust and Python daily"));
    }

    #[test]
    fn extract_personal_info() {
        let facts =
            MemoryManager::extract_facts_from_text("My name is Alice and I live in London.");
        assert!(facts.len() >= 2);
        let categories: Vec<_> = facts.iter().map(|(c, _)| c.clone()).collect();
        assert!(categories.contains(&semantic::FactCategory::PersonalInfo));
    }

    #[test]
    fn extract_goal() {
        let facts = MemoryManager::extract_facts_from_text("I'm trying to learn Japanese.");
        assert!(!facts.is_empty());
        assert!(matches!(facts[0].0, semantic::FactCategory::Goal));
    }

    #[test]
    fn extract_nothing_from_neutral_text() {
        let facts = MemoryManager::extract_facts_from_text("Hello, how are you?");
        assert!(facts.is_empty());
    }

    #[test]
    fn consolidate_extracts_facts_and_prunes() {
        let mm = test_memory();
        let uid = UserId("test_user".into());

        // Store several episodic entries with extractable content
        for i in 0..10 {
            let entry = episodic::EpisodicEntry {
                id: 0,
                user_id: "test_user".into(),
                content: format!("User: I prefer dark mode.\nAssistant: Noted! Entry {i}"),
                summary: None,
                channel: "cli".into(),
                timestamp: chrono::Utc::now(),
                embedding: None,
                relevance_score: 0.0,
            };
            mm.episodic.store(&entry).unwrap();
        }
        assert_eq!(mm.episodic.count(&uid).unwrap(), 10);

        // Consolidate with a 24-hour window, max 5 episodes
        let result = mm
            .consolidate_for_user(&uid, chrono::Duration::hours(24), 5)
            .unwrap();

        assert!(result.entries_scanned > 0);
        // Should have created at least 1 fact (preference extraction)
        assert!(result.facts_created >= 1);
        // Should have pruned episodes down to 5
        assert_eq!(result.episodes_pruned, 5);
        assert_eq!(mm.episodic.count(&uid).unwrap(), 5);
        // Should have at least 1 semantic fact
        assert!(mm.semantic.count(&uid).unwrap() >= 1);
    }

    #[test]
    fn consolidate_deduplicates_facts() {
        let mm = test_memory();
        let uid = UserId("dedup_user".into());

        // Store the same preference twice in different entries
        for _ in 0..2 {
            let entry = episodic::EpisodicEntry {
                id: 0,
                user_id: "dedup_user".into(),
                content: "I prefer dark mode.".into(),
                summary: None,
                channel: "cli".into(),
                timestamp: chrono::Utc::now(),
                embedding: None,
                relevance_score: 0.0,
            };
            mm.episodic.store(&entry).unwrap();
        }

        let result = mm
            .consolidate_for_user(&uid, chrono::Duration::hours(24), 100)
            .unwrap();

        // Only 1 fact should be created (dedup on second pass)
        assert_eq!(result.facts_created, 1);
        assert_eq!(mm.semantic.count(&uid).unwrap(), 1);
    }

    #[test]
    fn consolidate_empty_returns_default() {
        let mm = test_memory();
        let uid = UserId("empty_user".into());
        let result = mm
            .consolidate_for_user(&uid, chrono::Duration::hours(24), 100)
            .unwrap();
        assert_eq!(result.entries_scanned, 0);
        assert_eq!(result.facts_created, 0);
    }

    #[test]
    fn build_context_for_coding_uses_augmented_query() {
        let mm = test_memory();
        let uid = UserId("alice".into());
        let sid = ngenorca_core::SessionId::new();
        mm.episodic
            .store(&episodic::EpisodicEntry {
                id: 0,
                user_id: uid.0.clone(),
                content: "Previously fixed a rust cargo build script issue".into(),
                summary: None,
                channel: "webchat".into(),
                timestamp: chrono::Utc::now(),
                embedding: None,
                relevance_score: 0.0,
            })
            .unwrap();

        let classification = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: ngenorca_core::orchestration::TaskComplexity::Moderate,
            confidence: 0.9,
            method: ngenorca_core::orchestration::ClassificationMethod::RuleBased,
            domain_tags: vec!["rust".into()],
            language: Some("en".into()),
        };

        let ctx = mm
            .build_context_for_task(&uid, &sid, "fix build script", &classification, 512)
            .unwrap();
        assert!(!ctx.episodic_snippets.is_empty());
    }
}
