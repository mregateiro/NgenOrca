//! Learned routing rules — persisted rules derived from orchestration history.
//!
//! After each orchestration cycle, the system records the outcome. Over time,
//! patterns emerge: e.g. "Coding tasks with domain=rust always route to the coder
//! agent and get accepted with high quality." These patterns are distilled into
//! `LearnedRoutingRule`s.
//!
//! The `LearnedRouter` stores and retrieves these rules using SQLite, and
//! updates them incrementally from `OrchestrationRecord` events.

use chrono::Utc;
use ngenorca_core::Error;
use ngenorca_core::Result;
use ngenorca_core::orchestration::{
    LearnedRoutingRule, OrchestrationRecord, QualityVerdict, TaskComplexity, TaskIntent,
};
use rusqlite::{Connection, params};
use std::sync::Mutex;
use tracing::debug;

/// Persistent store for learned routing rules.
pub struct LearnedRouter {
    conn: Mutex<Connection>,
}

impl LearnedRouter {
    /// Create a new learned router backed by a SQLite database.
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path).map_err(|e| Error::Database(e.to_string()))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS learned_routes (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                intent      TEXT NOT NULL,
                domain      TEXT,
                max_complexity TEXT,
                target_agent TEXT NOT NULL,
                confidence  REAL NOT NULL DEFAULT 0.5,
                sample_count INTEGER NOT NULL DEFAULT 0,
                last_updated TEXT NOT NULL,
                UNIQUE(intent, domain, target_agent)
            );
            CREATE INDEX IF NOT EXISTS idx_routes_intent ON learned_routes(intent);",
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Look up the best learned routing rule for a given intent and optional domains.
    ///
    /// Returns the highest-confidence rule that matches the intent (and optionally a domain tag).
    pub fn lookup(
        &self,
        intent: &TaskIntent,
        domain_tags: &[String],
    ) -> Result<Option<LearnedRoutingRule>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let intent_str = serde_json::to_string(intent).unwrap_or_else(|_| "\"Unknown\"".into());

        // First try domain-specific match
        if !domain_tags.is_empty() {
            for domain in domain_tags {
                let result = conn.query_row(
                    "SELECT intent, domain, max_complexity, target_agent, confidence, sample_count, last_updated
                     FROM learned_routes
                     WHERE intent = ?1 AND domain = ?2 AND confidence >= 0.5
                     ORDER BY confidence DESC LIMIT 1",
                    params![intent_str, domain],
                    Self::row_to_rule,
                );
                if let Ok(rule) = result {
                    return Ok(Some(rule));
                }
            }
        }

        // Fall back to intent-only match (domain IS NULL)
        let result = conn.query_row(
            "SELECT intent, domain, max_complexity, target_agent, confidence, sample_count, last_updated
             FROM learned_routes
             WHERE intent = ?1 AND domain IS NULL AND confidence >= 0.5
             ORDER BY confidence DESC LIMIT 1",
            params![intent_str],
            Self::row_to_rule,
        );

        match result {
            Ok(rule) => Ok(Some(rule)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::Database(e.to_string())),
        }
    }

    /// Ingest an orchestration record and update routing rules.
    ///
    /// Only records with `Accept` quality verdicts contribute positively.
    /// Escalated records decrease confidence for the original agent.
    pub fn ingest(&self, record: &OrchestrationRecord) -> Result<()> {
        let intent_str = serde_json::to_string(&record.classification.intent)
            .unwrap_or_else(|_| "\"Unknown\"".into());
        let target = &record.routing.target.name;

        // Determine primary domain (first domain tag, if any)
        let domain: Option<&str> = record
            .classification
            .domain_tags
            .first()
            .map(|s| s.as_str());

        let accepted =
            matches!(record.quality, QualityVerdict::Accept { score: Some(s) } if s >= 0.6);
        let escalated = record.escalated;

        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        // Check if rule already exists
        let existing: Option<(i64, f64, u32)> = conn
            .query_row(
                "SELECT id, confidence, sample_count FROM learned_routes
                 WHERE intent = ?1 AND domain IS ?2 AND target_agent = ?3",
                params![intent_str, domain, target],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();

        if let Some((id, old_conf, old_count)) = existing {
            // Update existing rule with exponential moving average
            let new_count = old_count + 1;
            let alpha = 0.1_f64; // Learning rate
            let signal = if accepted && !escalated {
                1.0
            } else if escalated {
                0.2
            } else {
                0.5
            };
            let new_conf = (1.0 - alpha) * old_conf + alpha * signal;

            conn.execute(
                "UPDATE learned_routes SET confidence = ?1, sample_count = ?2, last_updated = ?3
                 WHERE id = ?4",
                params![new_conf, new_count, Utc::now().to_rfc3339(), id],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            debug!(
                intent = %intent_str,
                target = target,
                confidence = new_conf,
                samples = new_count,
                "Updated learned routing rule"
            );
        } else if accepted {
            // Create new rule only on successful outcomes
            let max_complexity = serde_json::to_string(&record.classification.complexity).ok();

            conn.execute(
                "INSERT OR IGNORE INTO learned_routes (intent, domain, max_complexity, target_agent, confidence, sample_count, last_updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    intent_str,
                    domain,
                    max_complexity,
                    target,
                    0.6, // Initial confidence for first success
                    1,
                    Utc::now().to_rfc3339(),
                ],
            ).map_err(|e| Error::Database(e.to_string()))?;

            debug!(
                intent = %intent_str,
                target = target,
                "Created new learned routing rule"
            );
        }

        Ok(())
    }

    /// Return all learned rules (for diagnostics / API).
    pub fn all_rules(&self) -> Result<Vec<LearnedRoutingRule>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT intent, domain, max_complexity, target_agent, confidence, sample_count, last_updated
             FROM learned_routes ORDER BY confidence DESC",
        ).map_err(|e| Error::Database(e.to_string()))?;

        let rules: Vec<LearnedRoutingRule> = stmt
            .query_map([], Self::row_to_rule)
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rules)
    }

    /// Count total learned rules.
    pub fn count(&self) -> Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learned_routes", [], |row| row.get(0))
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(count as u64)
    }

    fn row_to_rule(row: &rusqlite::Row) -> rusqlite::Result<LearnedRoutingRule> {
        let intent_json: String = row.get(0)?;
        let domain: Option<String> = row.get(1)?;
        let complexity_json: Option<String> = row.get(2)?;
        let target: String = row.get(3)?;
        let confidence: f64 = row.get(4)?;
        let sample_count: u32 = row.get(5)?;
        let updated_str: String = row.get(6)?;

        let intent: TaskIntent = serde_json::from_str(&intent_json).unwrap_or(TaskIntent::Unknown);
        let max_complexity: Option<TaskComplexity> =
            complexity_json.and_then(|s| serde_json::from_str(&s).ok());
        let last_updated = chrono::DateTime::parse_from_rfc3339(&updated_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        Ok(LearnedRoutingRule {
            intent,
            domain_filter: domain,
            max_complexity,
            target_agent: target,
            confidence,
            sample_count,
            last_updated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::orchestration::*;

    fn make_router() -> LearnedRouter {
        LearnedRouter::new(":memory:").unwrap()
    }

    fn make_record(
        intent: TaskIntent,
        target: &str,
        quality: QualityVerdict,
        escalated: bool,
    ) -> OrchestrationRecord {
        OrchestrationRecord {
            classification: TaskClassification {
                intent,
                complexity: TaskComplexity::Simple,
                confidence: 0.9,
                method: ClassificationMethod::RuleBased,
                domain_tags: vec![],
                language: None,
            },
            routing: RoutingDecision {
                target: SubAgentId {
                    name: target.into(),
                    model: "test-model".into(),
                },
                reason: "test".into(),
                system_prompt: String::new(),
                temperature: None,
                max_tokens: None,
                from_memory: false,
            },
            quality,
            quality_method: QualityMethod::Heuristic,
            escalated,
            latency_ms: 100,
            total_tokens: 200,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn empty_lookup_returns_none() {
        let router = make_router();
        assert!(router.lookup(&TaskIntent::Coding, &[]).unwrap().is_none());
    }

    #[test]
    fn ingest_creates_rule_on_accept() {
        let router = make_router();
        let record = make_record(
            TaskIntent::Coding,
            "coder",
            QualityVerdict::Accept { score: Some(0.8) },
            false,
        );
        router.ingest(&record).unwrap();
        assert_eq!(router.count().unwrap(), 1);

        let rule = router.lookup(&TaskIntent::Coding, &[]).unwrap().unwrap();
        assert_eq!(rule.target_agent, "coder");
        assert!(rule.confidence >= 0.5);
    }

    #[test]
    fn repeated_success_increases_confidence() {
        let router = make_router();
        for _ in 0..10 {
            let record = make_record(
                TaskIntent::Summarization,
                "summarizer",
                QualityVerdict::Accept { score: Some(0.9) },
                false,
            );
            router.ingest(&record).unwrap();
        }
        let rule = router
            .lookup(&TaskIntent::Summarization, &[])
            .unwrap()
            .unwrap();
        assert!(
            rule.confidence > 0.6,
            "Confidence should increase: {}",
            rule.confidence
        );
        assert_eq!(rule.sample_count, 10);
    }

    #[test]
    fn escalation_decreases_confidence() {
        let router = make_router();
        // First create with a success
        router
            .ingest(&make_record(
                TaskIntent::Analysis,
                "analyzer",
                QualityVerdict::Accept { score: Some(0.8) },
                false,
            ))
            .unwrap();

        let before = router
            .lookup(&TaskIntent::Analysis, &[])
            .unwrap()
            .unwrap()
            .confidence;

        // Then ingest an escalation
        router
            .ingest(&make_record(
                TaskIntent::Analysis,
                "analyzer",
                QualityVerdict::Escalate {
                    reason: "poor quality".into(),
                    escalate_to: None,
                },
                true,
            ))
            .unwrap();

        let after = router
            .lookup(&TaskIntent::Analysis, &[])
            .unwrap()
            .unwrap()
            .confidence;
        assert!(
            after < before,
            "Confidence should decrease: {} -> {}",
            before,
            after
        );
    }

    #[test]
    fn domain_specific_lookup() {
        let router = make_router();
        let mut record = make_record(
            TaskIntent::Coding,
            "rust-specialist",
            QualityVerdict::Accept { score: Some(0.85) },
            false,
        );
        record.classification.domain_tags = vec!["rust".into()];
        router.ingest(&record).unwrap();

        // Lookup with matching domain
        let rule = router
            .lookup(&TaskIntent::Coding, &["rust".into()])
            .unwrap()
            .unwrap();
        assert_eq!(rule.target_agent, "rust-specialist");

        // Lookup without domain should not find it (different row)
        let no_domain = router.lookup(&TaskIntent::Coding, &[]).unwrap();
        assert!(no_domain.is_none());
    }

    #[test]
    fn all_rules_returns_everything() {
        let router = make_router();
        router
            .ingest(&make_record(
                TaskIntent::Coding,
                "coder",
                QualityVerdict::Accept { score: Some(0.8) },
                false,
            ))
            .unwrap();
        router
            .ingest(&make_record(
                TaskIntent::Creative,
                "writer",
                QualityVerdict::Accept { score: Some(0.7) },
                false,
            ))
            .unwrap();
        let rules = router.all_rules().unwrap();
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn no_rule_created_on_low_quality() {
        let router = make_router();
        // Score below 0.6 threshold
        router
            .ingest(&make_record(
                TaskIntent::Coding,
                "coder",
                QualityVerdict::Accept { score: Some(0.4) },
                false,
            ))
            .unwrap();
        assert_eq!(router.count().unwrap(), 0);
    }
}
