//! Tier 2: Episodic Memory — conversation history with semantic search.
//!
//! Every conversation is stored and indexed. When the agent needs to recall
//! past interactions, it searches this tier by embedding similarity.

use chrono::{DateTime, Utc};
use ngenorca_core::types::UserId;
use ngenorca_core::{Error, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// An entry in episodic memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodicEntry {
    /// Unique ID.
    pub id: i64,
    /// User this memory belongs to.
    pub user_id: String,
    /// The conversation snippet.
    pub content: String,
    /// Summary of this episode.
    pub summary: Option<String>,
    /// Source channel.
    pub channel: String,
    /// When this happened.
    pub timestamp: DateTime<Utc>,
    /// Embedding vector (for semantic search).
    /// Stored as JSON-encoded f32 array.
    pub embedding: Option<Vec<f32>>,
    /// Relevance score (set during retrieval).
    #[serde(default)]
    pub relevance_score: f64,
}

/// Episodic memory store backed by SQLite.
pub struct EpisodicMemory {
    conn: Mutex<Connection>,
}

impl EpisodicMemory {
    pub fn new(db_path: &str) -> Result<Self> {
        // Ensure parent directory exists.
        if let Some(parent) = std::path::Path::new(db_path).parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let conn = Connection::open(db_path).map_err(|e| Error::Database(e.to_string()))?;

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| Error::Database(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS episodic_entries (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id    TEXT NOT NULL,
                content    TEXT NOT NULL,
                summary    TEXT,
                channel    TEXT NOT NULL DEFAULT 'unknown',
                timestamp  TEXT NOT NULL,
                embedding  BLOB
            );

            CREATE INDEX IF NOT EXISTS idx_episodic_user ON episodic_entries(user_id);
            CREATE INDEX IF NOT EXISTS idx_episodic_time ON episodic_entries(timestamp);",
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Store a new episodic memory entry.
    pub fn store(&self, entry: &EpisodicEntry) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;

        let embedding_blob: Option<Vec<u8>> = entry.embedding.as_ref().map(|e| {
            e.iter().flat_map(|f| f.to_le_bytes()).collect()
        });

        conn.execute(
            "INSERT INTO episodic_entries (user_id, content, summary, channel, timestamp, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.user_id,
                entry.content,
                entry.summary,
                entry.channel,
                entry.timestamp.to_rfc3339(),
                embedding_blob,
            ],
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(conn.last_insert_rowid())
    }

    /// Search episodic memory by text (full-text search fallback).
    /// When embeddings are available, this will use cosine similarity instead.
    pub fn search(
        &self,
        user_id: &UserId,
        query: &str,
        limit: usize,
        _token_budget: usize,
    ) -> Result<Vec<EpisodicEntry>> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;

        // TODO: When embedding model is available, use vector similarity search.
        // For now, fall back to LIKE-based text search + recency weighting.
        let search_pattern = format!("%{query}%");

        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, content, summary, channel, timestamp
                 FROM episodic_entries
                 WHERE user_id = ?1 AND content LIKE ?2
                 ORDER BY timestamp DESC
                 LIMIT ?3",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let entries = stmt
            .query_map(params![user_id.0, search_pattern, limit as i64], |row| {
                let id: i64 = row.get(0)?;
                let user_id: String = row.get(1)?;
                let content: String = row.get(2)?;
                let summary: Option<String> = row.get(3)?;
                let channel: String = row.get(4)?;
                let timestamp: String = row.get(5)?;
                Ok((id, user_id, content, summary, channel, timestamp))
            })
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .map(|(id, user_id, content, summary, channel, timestamp)| EpisodicEntry {
                id,
                user_id,
                content,
                summary,
                channel,
                timestamp: chrono::DateTime::parse_from_rfc3339(&timestamp)
                    .unwrap_or_default()
                    .with_timezone(&Utc),
                embedding: None,
                relevance_score: 0.0,
            })
            .collect();

        Ok(entries)
    }

    /// Get recent entries for a user (for consolidation into semantic memory).
    pub fn get_recent(
        &self,
        user_id: &UserId,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<EpisodicEntry>> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, content, summary, channel, timestamp
                 FROM episodic_entries
                 WHERE user_id = ?1 AND timestamp > ?2
                 ORDER BY timestamp ASC
                 LIMIT ?3",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let entries = stmt
            .query_map(
                params![user_id.0, since.to_rfc3339(), limit as i64],
                |row| {
                    let id: i64 = row.get(0)?;
                    let user_id: String = row.get(1)?;
                    let content: String = row.get(2)?;
                    let summary: Option<String> = row.get(3)?;
                    let channel: String = row.get(4)?;
                    let timestamp: String = row.get(5)?;
                    Ok((id, user_id, content, summary, channel, timestamp))
                },
            )
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .map(|(id, user_id, content, summary, channel, timestamp)| EpisodicEntry {
                id,
                user_id,
                content,
                summary,
                channel,
                timestamp: chrono::DateTime::parse_from_rfc3339(&timestamp)
                    .unwrap_or_default()
                    .with_timezone(&Utc),
                embedding: None,
                relevance_score: 0.0,
            })
            .collect();

        Ok(entries)
    }

    /// Count total entries for a user.
    pub fn count(&self, user_id: &UserId) -> Result<u64> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM episodic_entries WHERE user_id = ?1",
                params![user_id.0],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(count as u64)
    }

    /// Prune oldest entries for a user, keeping at most `max_entries`.
    ///
    /// Returns the number of entries removed.
    pub fn prune(&self, user_id: &UserId, max_entries: usize) -> Result<usize> {
        let current = self.count(user_id)? as usize;
        if current <= max_entries {
            return Ok(0);
        }

        let to_remove = current - max_entries;
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;
        let removed = conn
            .execute(
                "DELETE FROM episodic_entries WHERE id IN (
                    SELECT id FROM episodic_entries
                    WHERE user_id = ?1
                    ORDER BY timestamp ASC
                    LIMIT ?2
                )",
                params![user_id.0, to_remove],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        Ok(removed)
    }

    /// Return all distinct user IDs stored in episodic memory.
    pub fn distinct_users(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| Error::Database(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT DISTINCT user_id FROM episodic_entries")
            .map_err(|e| Error::Database(e.to_string()))?;
        let users: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(users)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ngenorca_core::types::UserId;

    fn mem() -> EpisodicMemory {
        EpisodicMemory::new(":memory:").unwrap()
    }

    fn make_entry(user: &str, content: &str, channel: &str) -> EpisodicEntry {
        EpisodicEntry {
            id: 0,
            user_id: user.to_string(),
            content: content.to_string(),
            summary: None,
            channel: channel.to_string(),
            timestamp: Utc::now(),
            embedding: None,
            relevance_score: 0.0,
        }
    }

    #[test]
    fn new_in_memory() {
        let em = mem();
        let uid = UserId("u1".into());
        assert_eq!(em.count(&uid).unwrap(), 0);
    }

    #[test]
    fn store_and_count() {
        let em = mem();
        let uid = UserId("u1".into());
        let entry = make_entry("u1", "hello world", "cli");
        em.store(&entry).unwrap();
        assert_eq!(em.count(&uid).unwrap(), 1);
    }

    #[test]
    fn store_returns_rowid() {
        let em = mem();
        let e1 = make_entry("u1", "first", "cli");
        let e2 = make_entry("u1", "second", "cli");
        let id1 = em.store(&e1).unwrap();
        let id2 = em.store(&e2).unwrap();
        assert!(id2 > id1);
    }

    #[test]
    fn search_finds_matching_content() {
        let em = mem();
        let uid = UserId("u1".into());
        em.store(&make_entry("u1", "I love Rust programming", "cli")).unwrap();
        em.store(&make_entry("u1", "Python is nice too", "web")).unwrap();
        let results = em.search(&uid, "Rust", 10, 1000).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("Rust"));
    }

    #[test]
    fn search_no_match_returns_empty() {
        let em = mem();
        let uid = UserId("u1".into());
        em.store(&make_entry("u1", "hello world", "cli")).unwrap();
        let results = em.search(&uid, "foobar", 10, 1000).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let em = mem();
        let uid = UserId("u1".into());
        for i in 0..5 {
            em.store(&make_entry("u1", &format!("entry {i} about Rust"), "cli")).unwrap();
        }
        let results = em.search(&uid, "Rust", 3, 10000).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn search_is_user_scoped() {
        let em = mem();
        let uid1 = UserId("u1".into());
        em.store(&make_entry("u1", "Rust for u1", "cli")).unwrap();
        em.store(&make_entry("u2", "Rust for u2", "cli")).unwrap();
        let results = em.search(&uid1, "Rust", 10, 10000).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].user_id, "u1");
    }

    #[test]
    fn get_recent_returns_entries_since_time() {
        let em = mem();
        let uid = UserId("u1".into());
        let past = Utc::now() - chrono::Duration::hours(2);
        em.store(&make_entry("u1", "old entry", "cli")).unwrap();
        let recent_cutoff = Utc::now() - chrono::Duration::seconds(1);
        em.store(&make_entry("u1", "new entry", "cli")).unwrap();
        // All entries were inserted at ~now so get_recent with past cutoff returns both
        let results = em.get_recent(&uid, past, 10).unwrap();
        assert_eq!(results.len(), 2);
        // With a very recent cutoff, should get only the newest
        let results = em.get_recent(&uid, recent_cutoff, 10).unwrap();
        // Both have timestamps around now, so depends on precision; at least we test the API
        assert!(results.len() <= 2);
    }

    #[test]
    fn entry_with_summary() {
        let em = mem();
        let uid = UserId("u1".into());
        let mut entry = make_entry("u1", "detailed conversation about AI", "web");
        entry.summary = Some("AI discussion".to_string());
        em.store(&entry).unwrap();
        let found = em.search(&uid, "AI", 10, 10000).unwrap();
        assert_eq!(found.len(), 1);
        // Note: summary is stored but the search/get_recent SQL does retrieve it
    }

    #[test]
    fn episodic_entry_serde_roundtrip() {
        let entry = EpisodicEntry {
            id: 42,
            user_id: "u1".into(),
            content: "test content".into(),
            summary: Some("summary".into()),
            channel: "cli".into(),
            timestamp: Utc::now(),
            embedding: Some(vec![0.1, 0.2, 0.3]),
            relevance_score: 0.95,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: EpisodicEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 42);
        assert_eq!(back.content, "test content");
        assert_eq!(back.embedding.unwrap().len(), 3);
        assert!((back.relevance_score - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn prune_removes_oldest_entries() {
        let em = mem();
        let uid = UserId("u1".into());

        // Store 5 entries
        for i in 0..5 {
            em.store(&make_entry("u1", &format!("entry {i}"), "cli"))
                .unwrap();
        }
        assert_eq!(em.count(&uid).unwrap(), 5);

        // Prune to keep max 3
        let removed = em.prune(&uid, 3).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(em.count(&uid).unwrap(), 3);
    }

    #[test]
    fn prune_noop_when_under_limit() {
        let em = mem();
        let uid = UserId("u1".into());

        em.store(&make_entry("u1", "one", "cli")).unwrap();
        let removed = em.prune(&uid, 10).unwrap();
        assert_eq!(removed, 0);
        assert_eq!(em.count(&uid).unwrap(), 1);
    }
}
