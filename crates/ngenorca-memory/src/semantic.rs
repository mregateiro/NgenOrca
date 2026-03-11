//! Tier 3: Semantic Memory — distilled facts, preferences, and knowledge.
//!
//! Background consolidation extracts structured facts from episodic memory
//! and maintains a compact, always-current knowledge base per user.

use chrono::{DateTime, Utc};
use ngenorca_core::types::UserId;
use ngenorca_core::{Error, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// A distilled fact in semantic memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticFact {
    /// Unique fact ID.
    pub id: i64,
    /// User this fact belongs to.
    pub user_id: String,
    /// Category of the fact.
    pub category: FactCategory,
    /// The fact itself (natural language).
    pub fact: String,
    /// Confidence score (0.0 - 1.0).
    pub confidence: f64,
    /// Source: which episodic entries contributed to this fact.
    pub source_episode_ids: Vec<i64>,
    /// When this fact was first established.
    pub established_at: DateTime<Utc>,
    /// When this fact was last confirmed/updated.
    pub last_confirmed: DateTime<Utc>,
    /// Access count (how often this fact has been retrieved).
    pub access_count: u32,
}

/// Categories of semantic facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactCategory {
    /// User preferences (e.g., "prefers dark mode", "likes Italian food").
    Preference,
    /// Personal information (e.g., "lives in NYC", "works at Acme Corp").
    PersonalInfo,
    /// Relationships (e.g., "Sarah is their sister").
    Relationship,
    /// Habits/routines (e.g., "usually goes to gym on Mondays").
    Routine,
    /// Technical preferences (e.g., "prefers Rust over Go").
    TechnicalPreference,
    /// Important dates (e.g., "birthday is March 15").
    ImportantDate,
    /// Goals/tasks (e.g., "planning a trip to Japan").
    Goal,
    /// General knowledge (e.g., "their WiFi password is ...").
    Knowledge,
    /// Other.
    Other(String),
}

/// Semantic memory store.
pub struct SemanticMemory {
    conn: Mutex<Connection>,
}

impl SemanticMemory {
    pub fn new(db_path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(db_path).parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let conn = Connection::open(db_path).map_err(|e| Error::Database(e.to_string()))?;

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| Error::Database(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS semantic_facts (
                id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id            TEXT NOT NULL,
                category           TEXT NOT NULL,
                fact               TEXT NOT NULL,
                confidence         REAL NOT NULL DEFAULT 0.5,
                source_episode_ids TEXT NOT NULL DEFAULT '[]',
                established_at     TEXT NOT NULL,
                last_confirmed     TEXT NOT NULL,
                access_count       INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_semantic_user ON semantic_facts(user_id);
            CREATE INDEX IF NOT EXISTS idx_semantic_category ON semantic_facts(user_id, category);
            CREATE INDEX IF NOT EXISTS idx_semantic_confidence ON semantic_facts(user_id, confidence);",
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Store a new semantic fact.
    pub fn store_fact(&self, fact: &SemanticFact) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;

        let source_ids = serde_json::to_string(&fact.source_episode_ids)?;
        let category = serde_json::to_string(&fact.category)?;

        conn.execute(
            "INSERT INTO semantic_facts
             (user_id, category, fact, confidence, source_episode_ids, established_at, last_confirmed, access_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                fact.user_id,
                category,
                fact.fact,
                fact.confidence,
                source_ids,
                fact.established_at.to_rfc3339(),
                fact.last_confirmed.to_rfc3339(),
                fact.access_count,
            ],
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(conn.last_insert_rowid())
    }

    /// Retrieve all facts for a user, ordered by confidence and recency.
    /// Respects the token budget by estimating ~4 chars per token.
    pub fn retrieve_for_user(
        &self,
        user_id: &UserId,
        token_budget: usize,
    ) -> Result<Vec<SemanticFact>> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, category, fact, confidence,
                        source_episode_ids, established_at, last_confirmed, access_count
                 FROM semantic_facts
                 WHERE user_id = ?1 AND confidence > 0.3
                 ORDER BY confidence DESC, last_confirmed DESC",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let facts: Vec<SemanticFact> = stmt
            .query_map(params![user_id.0], |row| {
                let id: i64 = row.get(0)?;
                let user_id: String = row.get(1)?;
                let category: String = row.get(2)?;
                let fact: String = row.get(3)?;
                let confidence: f64 = row.get(4)?;
                let source_ids: String = row.get(5)?;
                let established_at: String = row.get(6)?;
                let last_confirmed: String = row.get(7)?;
                let access_count: u32 = row.get(8)?;
                Ok((id, user_id, category, fact, confidence, source_ids, established_at, last_confirmed, access_count))
            })
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .map(|(id, user_id, category, fact, confidence, source_ids, established_at, last_confirmed, access_count)| {
                SemanticFact {
                    id,
                    user_id,
                    category: serde_json::from_str(&category).unwrap_or(FactCategory::Other("unknown".into())),
                    fact,
                    confidence,
                    source_episode_ids: serde_json::from_str(&source_ids).unwrap_or_default(),
                    established_at: chrono::DateTime::parse_from_rfc3339(&established_at)
                        .unwrap_or_default()
                        .with_timezone(&chrono::Utc),
                    last_confirmed: chrono::DateTime::parse_from_rfc3339(&last_confirmed)
                        .unwrap_or_default()
                        .with_timezone(&chrono::Utc),
                    access_count,
                }
            })
            .collect();

        // Trim to fit token budget (~4 chars per token).
        let char_budget = token_budget * 4;
        let mut total_chars = 0;
        let mut result = Vec::new();

        for fact in facts {
            total_chars += fact.fact.len() + 20; // +20 for category label overhead
            if total_chars > char_budget {
                break;
            }
            result.push(fact);
        }

        // Update access counts for retrieved facts.
        drop(stmt);
        for fact in &result {
            conn.execute(
                "UPDATE semantic_facts SET access_count = access_count + 1 WHERE id = ?1",
                params![fact.id],
            )
            .ok();
        }

        Ok(result)
    }

    /// Update a fact's confidence (e.g., when confirmed by new conversation).
    pub fn update_confidence(&self, fact_id: i64, new_confidence: f64) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;
        conn.execute(
            "UPDATE semantic_facts SET confidence = ?1, last_confirmed = ?2 WHERE id = ?3",
            params![new_confidence, Utc::now().to_rfc3339(), fact_id],
        )
        .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }

    /// Prune low-confidence, stale facts.
    pub fn prune(&self, user_id: &UserId, min_confidence: f64, max_age_days: i64) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;

        let cutoff = (Utc::now() - chrono::Duration::days(max_age_days)).to_rfc3339();

        let count = conn
            .execute(
                "DELETE FROM semantic_facts
                 WHERE user_id = ?1 AND confidence < ?2 AND last_confirmed < ?3",
                params![user_id.0, min_confidence, cutoff],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        Ok(count)
    }

    /// Count total facts for a user.
    pub fn count(&self, user_id: &UserId) -> Result<u64> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM semantic_facts WHERE user_id = ?1",
                params![user_id.0],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(count as u64)
    }

    /// PRIV-03: Delete **all** semantic facts for a user.
    ///
    /// Returns the number of rows deleted.
    pub fn delete_for_user(&self, user_id: &UserId) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;
        let deleted = conn
            .execute(
                "DELETE FROM semantic_facts WHERE user_id = ?1",
                params![user_id.0],
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ngenorca_core::types::UserId;

    fn mem() -> SemanticMemory {
        SemanticMemory::new(":memory:").unwrap()
    }

    fn make_fact(user: &str, category: FactCategory, fact: &str, confidence: f64) -> SemanticFact {
        let now = Utc::now();
        SemanticFact {
            id: 0,
            user_id: user.to_string(),
            category,
            fact: fact.to_string(),
            confidence,
            source_episode_ids: vec![1, 2],
            established_at: now,
            last_confirmed: now,
            access_count: 0,
        }
    }

    #[test]
    fn new_in_memory() {
        let sm = mem();
        let uid = UserId("u1".into());
        assert_eq!(sm.count(&uid).unwrap(), 0);
    }

    #[test]
    fn store_and_count() {
        let sm = mem();
        let uid = UserId("u1".into());
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "likes Rust", 0.9)).unwrap();
        assert_eq!(sm.count(&uid).unwrap(), 1);
    }

    #[test]
    fn store_returns_incrementing_ids() {
        let sm = mem();
        let id1 = sm.store_fact(&make_fact("u1", FactCategory::Preference, "fact1", 0.8)).unwrap();
        let id2 = sm.store_fact(&make_fact("u1", FactCategory::PersonalInfo, "fact2", 0.7)).unwrap();
        assert!(id2 > id1);
    }

    #[test]
    fn retrieve_for_user_returns_facts() {
        let sm = mem();
        let uid = UserId("u1".into());
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "likes dark mode", 0.9)).unwrap();
        sm.store_fact(&make_fact("u1", FactCategory::PersonalInfo, "lives in Lisbon", 0.8)).unwrap();
        let facts = sm.retrieve_for_user(&uid, 10000).unwrap();
        assert_eq!(facts.len(), 2);
    }

    #[test]
    fn retrieve_filters_low_confidence() {
        let sm = mem();
        let uid = UserId("u1".into());
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "high conf", 0.9)).unwrap();
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "low conf", 0.2)).unwrap();
        let facts = sm.retrieve_for_user(&uid, 10000).unwrap();
        // Only confidence > 0.3 returned
        assert_eq!(facts.len(), 1);
        assert!(facts[0].fact.contains("high conf"));
    }

    #[test]
    fn retrieve_is_user_scoped() {
        let sm = mem();
        let uid1 = UserId("u1".into());
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "u1 fact", 0.9)).unwrap();
        sm.store_fact(&make_fact("u2", FactCategory::Preference, "u2 fact", 0.9)).unwrap();
        let facts = sm.retrieve_for_user(&uid1, 10000).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].user_id, "u1");
    }

    #[test]
    fn retrieve_respects_token_budget() {
        let sm = mem();
        let uid = UserId("u1".into());
        for i in 0..20 {
            sm.store_fact(&make_fact(
                "u1",
                FactCategory::Knowledge,
                &format!("This is knowledge fact number {i} with enough content"),
                0.9,
            )).unwrap();
        }
        // Budget of 10 tokens = ~40 chars, should limit results
        let facts = sm.retrieve_for_user(&uid, 10).unwrap();
        assert!(facts.len() < 20);
    }

    #[test]
    fn retrieve_orders_by_confidence_desc() {
        let sm = mem();
        let uid = UserId("u1".into());
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "low", 0.5)).unwrap();
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "high", 0.99)).unwrap();
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "mid", 0.7)).unwrap();
        let facts = sm.retrieve_for_user(&uid, 10000).unwrap();
        assert_eq!(facts[0].fact, "high");
        assert_eq!(facts[1].fact, "mid");
        assert_eq!(facts[2].fact, "low");
    }

    #[test]
    fn update_confidence() {
        let sm = mem();
        let uid = UserId("u1".into());
        let id = sm.store_fact(&make_fact("u1", FactCategory::Preference, "test", 0.5)).unwrap();
        sm.update_confidence(id, 0.95).unwrap();
        let facts = sm.retrieve_for_user(&uid, 10000).unwrap();
        assert!((facts[0].confidence - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn prune_removes_low_confidence_old_facts() {
        let sm = mem();
        let uid = UserId("u1".into());
        // Store a low-confidence fact (last_confirmed ≈ now)
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "stale", 0.1)).unwrap();
        // Store a high-confidence fact
        sm.store_fact(&make_fact("u1", FactCategory::Preference, "solid", 0.9)).unwrap();
        assert_eq!(sm.count(&uid).unwrap(), 2);
        // Prune with max_age_days=0 → cutoff ≈ now, so the low-conf fact qualifies
        let removed = sm.prune(&uid, 0.5, 0).unwrap();
        assert_eq!(removed, 1);
        // Only the high-confidence fact remains
        assert_eq!(sm.count(&uid).unwrap(), 1);
    }

    #[test]
    fn fact_category_serde_roundtrip() {
        let cats = vec![
            FactCategory::Preference,
            FactCategory::PersonalInfo,
            FactCategory::Relationship,
            FactCategory::Routine,
            FactCategory::TechnicalPreference,
            FactCategory::ImportantDate,
            FactCategory::Goal,
            FactCategory::Knowledge,
            FactCategory::Other("custom".into()),
        ];
        for cat in cats {
            let json = serde_json::to_string(&cat).unwrap();
            let back: FactCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cat);
        }
    }

    #[test]
    fn semantic_fact_serde_roundtrip() {
        let fact = make_fact("u1", FactCategory::TechnicalPreference, "prefers Rust", 0.95);
        let json = serde_json::to_string(&fact).unwrap();
        let back: SemanticFact = serde_json::from_str(&json).unwrap();
        assert_eq!(back.user_id, "u1");
        assert_eq!(back.fact, "prefers Rust");
        assert!((back.confidence - 0.95).abs() < f64::EPSILON);
        assert_eq!(back.source_episode_ids, vec![1, 2]);
    }
}
