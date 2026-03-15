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
use ngenorca_config::LearnedRoutingConfig;
use ngenorca_core::Error;
use ngenorca_core::Result;
use ngenorca_core::orchestration::{
    LearnedRoutingRule, OrchestrationRecord, QualityVerdict, TaskClassification, TaskComplexity,
    TaskIntent,
};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::sync::Mutex;
use tracing::debug;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearnedRouteDiagnostics {
    pub rule: LearnedRoutingRule,
    pub effective_confidence: f64,
    pub age_days: u32,
    pub staleness_penalty: f64,
    pub adaptive_decay_multiplier: f64,
    pub outcome_trend_adjustment: f64,
    pub accept_rate: f64,
    pub escalation_rate: f64,
    pub failure_rate: f64,
    pub stability_score: f64,
    pub stale: bool,
    pub accept_count: u32,
    pub escalation_count: u32,
    pub failure_count: u32,
    pub last_outcome: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearnedRouteHistorySummary {
    pub total_rules: usize,
    pub eligible_rules: usize,
    pub penalized_rules: usize,
    pub stale_rules: usize,
    pub intents: Vec<LearnedRouteBucketSummary>,
    pub agents: Vec<LearnedRouteBucketSummary>,
    pub domains: Vec<LearnedRouteBucketSummary>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LearnedRouteBucketSummary {
    pub label: String,
    pub total_rules: usize,
    pub eligible_rules: usize,
    pub accepted_samples: u32,
    pub escalated_samples: u32,
    pub failed_samples: u32,
    pub avg_effective_confidence: f64,
    pub avg_age_days: f64,
}

#[derive(Debug, Default, Clone)]
struct BucketAccumulator {
    total_rules: usize,
    eligible_rules: usize,
    accepted_samples: u32,
    escalated_samples: u32,
    failed_samples: u32,
    total_effective_confidence: f64,
    total_age_days: u64,
}

#[derive(Debug, Clone)]
struct StoredRule {
    rule: LearnedRoutingRule,
    accept_count: u32,
    escalation_count: u32,
    failure_count: u32,
    last_outcome: String,
}

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
                accept_count INTEGER NOT NULL DEFAULT 0,
                escalation_count INTEGER NOT NULL DEFAULT 0,
                failure_count INTEGER NOT NULL DEFAULT 0,
                last_outcome TEXT NOT NULL DEFAULT 'accepted',
                last_updated TEXT NOT NULL,
                UNIQUE(intent, domain, target_agent)
            );
            CREATE INDEX IF NOT EXISTS idx_routes_intent ON learned_routes(intent);",
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        let _ = conn.execute(
            "ALTER TABLE learned_routes ADD COLUMN accept_count INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE learned_routes ADD COLUMN escalation_count INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE learned_routes ADD COLUMN failure_count INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE learned_routes ADD COLUMN last_outcome TEXT NOT NULL DEFAULT 'accepted'",
            [],
        );

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
        Ok(self
            .lookup_diagnostic(intent, domain_tags)?
            .map(|diagnostic| diagnostic.rule))
    }

    pub fn lookup_diagnostic(
        &self,
        intent: &TaskIntent,
        domain_tags: &[String],
    ) -> Result<Option<LearnedRouteDiagnostics>> {
        self.lookup_diagnostic_with_policy(intent, domain_tags, &LearnedRoutingConfig::default())
    }

    pub fn lookup_diagnostic_with_policy(
        &self,
        intent: &TaskIntent,
        domain_tags: &[String],
        policy: &LearnedRoutingConfig,
    ) -> Result<Option<LearnedRouteDiagnostics>> {
        if !policy.enabled {
            return Ok(None);
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let rules = Self::load_rules_for_intent(&conn, intent)?;

        let mut candidates: Vec<LearnedRouteDiagnostics> = rules
            .iter()
            .filter(|rule| {
                rule.rule
                    .domain_filter
                    .as_ref()
                    .is_some_and(|domain| domain_tags.iter().any(|tag| tag == domain))
            })
            .map(|rule| Self::to_diagnostic_with_policy(rule, policy))
            .filter(|diagnostic| Self::rule_is_eligible(diagnostic, policy))
            .collect();

        if candidates.is_empty() {
            candidates = rules
                .iter()
                .filter(|rule| rule.rule.domain_filter.is_none())
                .map(|rule| Self::to_diagnostic_with_policy(rule, policy))
                .filter(|diagnostic| Self::rule_is_eligible(diagnostic, policy))
                .collect();
        }

        Ok(candidates.into_iter().max_by(|left, right| {
            left.effective_confidence
                .partial_cmp(&right.effective_confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    left.stability_score
                        .partial_cmp(&right.stability_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    left.accept_rate
                        .partial_cmp(&right.accept_rate)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| left.rule.sample_count.cmp(&right.rule.sample_count))
        }))
    }

    pub fn lookup_for_task_with_policy(
        &self,
        classification: &TaskClassification,
        policy: &LearnedRoutingConfig,
    ) -> Result<Option<LearnedRouteDiagnostics>> {
        if !policy.enabled {
            return Ok(None);
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let rules = Self::load_rules_for_intent(&conn, &classification.intent)?;

        let select = |prefer_domain_match: bool| {
            rules
                .iter()
                .filter(|rule| {
                    complexity_supported(rule.rule.max_complexity, classification.complexity)
                        && (!prefer_domain_match
                            || rule.rule.domain_filter.as_ref().is_some_and(|domain| {
                                classification.domain_tags.iter().any(|tag| tag == domain)
                            }))
                })
                .map(|rule| Self::to_diagnostic_with_policy(rule, policy))
                .filter(|diagnostic| Self::rule_is_eligible(diagnostic, policy))
                .max_by(|left, right| {
                    learned_route_candidate_score(left, classification)
                        .partial_cmp(&learned_route_candidate_score(right, classification))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| left.rule.sample_count.cmp(&right.rule.sample_count))
                })
        };

        Ok(select(true).or_else(|| {
            rules
                .iter()
                .filter(|rule| {
                    rule.rule.domain_filter.is_none()
                        && complexity_supported(rule.rule.max_complexity, classification.complexity)
                })
                .map(|rule| Self::to_diagnostic_with_policy(rule, policy))
                .filter(|diagnostic| Self::rule_is_eligible(diagnostic, policy))
                .max_by(|left, right| {
                    learned_route_candidate_score(left, classification)
                        .partial_cmp(&learned_route_candidate_score(right, classification))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| left.rule.sample_count.cmp(&right.rule.sample_count))
                })
        }))
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
        let contradiction_penalty = (record.synthesis.contradiction_score * 0.08).clamp(0.0, 0.08);
        let (signal, last_outcome) = if accepted && !escalated {
            if record.correction.grounded && record.correction.remediation_succeeded {
                (0.98 - contradiction_penalty, "accepted_recovered")
            } else if record.correction.grounded {
                (0.94 - contradiction_penalty, "accepted_verified")
            } else if record.correction.verification_attempted {
                (0.74 - contradiction_penalty, "accepted_unverified")
            } else {
                (1.0 - contradiction_penalty, "accepted")
            }
        } else if escalated {
            (0.1, "escalated")
        } else {
            (0.0, "failed")
        };

        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        // Check if rule already exists
        let existing: Option<(i64, f64, u32, u32, u32, u32)> = conn
            .query_row(
                "SELECT id, confidence, sample_count, accept_count, escalation_count, failure_count FROM learned_routes
                 WHERE intent = ?1 AND domain IS ?2 AND target_agent = ?3",
                params![intent_str, domain, target],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .ok();

        if let Some((id, old_conf, old_count, old_accepts, old_escalations, old_failures)) =
            existing
        {
            // Update existing rule with exponential moving average
            let new_count = old_count + 1;
            let alpha = 0.12_f64; // Learning rate
            let new_conf = (1.0 - alpha) * old_conf + alpha * signal;
            let new_accepts = old_accepts + u32::from(accepted && !escalated);
            let new_escalations = old_escalations + u32::from(escalated);
            let new_failures = old_failures + u32::from(!accepted && !escalated);

            conn.execute(
                "UPDATE learned_routes
                 SET confidence = ?1,
                     sample_count = ?2,
                     accept_count = ?3,
                     escalation_count = ?4,
                     failure_count = ?5,
                     last_outcome = ?6,
                     last_updated = ?7
                 WHERE id = ?8",
                params![
                    new_conf,
                    new_count,
                    new_accepts,
                    new_escalations,
                    new_failures,
                    last_outcome,
                    record.timestamp.to_rfc3339(),
                    id,
                ],
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
            let initial_confidence = signal.max(0.6);

            conn.execute(
                "INSERT OR IGNORE INTO learned_routes (
                    intent,
                    domain,
                    max_complexity,
                    target_agent,
                    confidence,
                    sample_count,
                    accept_count,
                    escalation_count,
                    failure_count,
                    last_outcome,
                    last_updated
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    intent_str,
                    domain,
                    max_complexity,
                    target,
                    initial_confidence,
                    1,
                    1,
                    0,
                    0,
                    last_outcome,
                    record.timestamp.to_rfc3339(),
                ],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

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
        Ok(self
            .diagnostics()?
            .into_iter()
            .map(|diagnostic| diagnostic.rule)
            .collect())
    }

    pub fn diagnostics(&self) -> Result<Vec<LearnedRouteDiagnostics>> {
        self.diagnostics_with_policy(&LearnedRoutingConfig::default(), false)
    }

    pub fn diagnostics_with_policy(
        &self,
        policy: &LearnedRoutingConfig,
        include_penalized: bool,
    ) -> Result<Vec<LearnedRouteDiagnostics>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                "SELECT intent, domain, max_complexity, target_agent, confidence, sample_count,
                        accept_count, escalation_count, failure_count, last_outcome, last_updated
                 FROM learned_routes",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let mut rules: Vec<LearnedRouteDiagnostics> = stmt
            .query_map([], Self::row_to_stored_rule)
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|row| row.ok())
            .map(|rule| Self::to_diagnostic_with_policy(&rule, policy))
            .filter(|diagnostic| include_penalized || Self::rule_is_eligible(diagnostic, policy))
            .collect();

        rules.sort_by(|left, right| {
            right
                .effective_confidence
                .partial_cmp(&left.effective_confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.rule.sample_count.cmp(&left.rule.sample_count))
        });

        Ok(rules)
    }

    pub fn history_summary_with_policy(
        &self,
        policy: &LearnedRoutingConfig,
        include_penalized: bool,
    ) -> Result<LearnedRouteHistorySummary> {
        let diagnostics = self.diagnostics_with_policy(policy, true)?;

        let total_rules = diagnostics.len();
        let eligible_rules = diagnostics
            .iter()
            .filter(|diagnostic| Self::rule_is_eligible(diagnostic, policy))
            .count();
        let stale_rules = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.stale)
            .count();
        let penalized_rules = diagnostics
            .iter()
            .filter(|diagnostic| !Self::rule_is_eligible(diagnostic, policy))
            .count();

        let visible = if include_penalized {
            diagnostics
        } else {
            diagnostics
                .into_iter()
                .filter(|diagnostic| Self::rule_is_eligible(diagnostic, policy))
                .collect()
        };

        let mut intents = BTreeMap::<String, BucketAccumulator>::new();
        let mut agents = BTreeMap::<String, BucketAccumulator>::new();
        let mut domains = BTreeMap::<String, BucketAccumulator>::new();

        for diagnostic in &visible {
            let eligible = Self::rule_is_eligible(diagnostic, policy);
            update_bucket(
                intents
                    .entry(format!("{:?}", diagnostic.rule.intent))
                    .or_default(),
                diagnostic,
                eligible,
            );
            update_bucket(
                agents
                    .entry(diagnostic.rule.target_agent.clone())
                    .or_default(),
                diagnostic,
                eligible,
            );
            update_bucket(
                domains
                    .entry(
                        diagnostic
                            .rule
                            .domain_filter
                            .clone()
                            .unwrap_or_else(|| "(none)".into()),
                    )
                    .or_default(),
                diagnostic,
                eligible,
            );
        }

        Ok(LearnedRouteHistorySummary {
            total_rules,
            eligible_rules,
            penalized_rules,
            stale_rules,
            intents: buckets_to_summary(intents),
            agents: buckets_to_summary(agents),
            domains: buckets_to_summary(domains),
        })
    }

    pub fn delete_rules(
        &self,
        intent: Option<&TaskIntent>,
        target_agent: Option<&str>,
    ) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let deleted = match (intent, target_agent) {
            (Some(intent), Some(target_agent)) => conn.execute(
                "DELETE FROM learned_routes WHERE intent = ?1 AND target_agent = ?2",
                params![Self::intent_key(intent), target_agent],
            ),
            (Some(intent), None) => conn.execute(
                "DELETE FROM learned_routes WHERE intent = ?1",
                params![Self::intent_key(intent)],
            ),
            (None, Some(target_agent)) => conn.execute(
                "DELETE FROM learned_routes WHERE target_agent = ?1",
                params![target_agent],
            ),
            (None, None) => conn.execute("DELETE FROM learned_routes", []),
        }
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(deleted)
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

    fn load_rules_for_intent(conn: &Connection, intent: &TaskIntent) -> Result<Vec<StoredRule>> {
        let intent_str = Self::intent_key(intent);
        let mut stmt = conn
            .prepare(
                "SELECT intent, domain, max_complexity, target_agent, confidence, sample_count,
                        accept_count, escalation_count, failure_count, last_outcome, last_updated
                 FROM learned_routes
                 WHERE intent = ?1",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let rules = stmt
            .query_map(params![intent_str], Self::row_to_stored_rule)
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|row| row.ok())
            .collect();

        Ok(rules)
    }

    fn row_to_stored_rule(row: &rusqlite::Row) -> rusqlite::Result<StoredRule> {
        let intent_json: String = row.get(0)?;
        let domain: Option<String> = row.get(1)?;
        let complexity_json: Option<String> = row.get(2)?;
        let target: String = row.get(3)?;
        let confidence: f64 = row.get(4)?;
        let sample_count: u32 = row.get(5)?;
        let accept_count: u32 = row.get(6)?;
        let escalation_count: u32 = row.get(7)?;
        let failure_count: u32 = row.get(8)?;
        let last_outcome: String = row.get(9)?;
        let updated_str: String = row.get(10)?;

        let intent: TaskIntent = serde_json::from_str(&intent_json).unwrap_or(TaskIntent::Unknown);
        let max_complexity: Option<TaskComplexity> =
            complexity_json.and_then(|s| serde_json::from_str(&s).ok());
        let last_updated = chrono::DateTime::parse_from_rfc3339(&updated_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        Ok(StoredRule {
            rule: LearnedRoutingRule {
                intent,
                domain_filter: domain,
                max_complexity,
                target_agent: target,
                confidence,
                sample_count,
                last_updated,
            },
            accept_count,
            escalation_count,
            failure_count,
            last_outcome,
        })
    }

    fn to_diagnostic_with_policy(
        rule: &StoredRule,
        policy: &LearnedRoutingConfig,
    ) -> LearnedRouteDiagnostics {
        let age_days = age_days(rule.rule.last_updated);
        let (accept_rate, escalation_rate, failure_rate) =
            routing_rates(rule.accept_count, rule.escalation_count, rule.failure_count);
        let stability_score = stability_score(accept_rate, escalation_rate, failure_rate);
        let adaptive_decay_multiplier = adaptive_decay_multiplier(
            age_days,
            rule.accept_count,
            rule.escalation_count,
            rule.failure_count,
            &rule.last_outcome,
            policy,
        );
        let staleness_penalty = staleness_penalty(age_days, policy) * adaptive_decay_multiplier;
        let outcome_trend_adjustment = outcome_trend_adjustment(
            age_days,
            rule.accept_count,
            rule.escalation_count,
            rule.failure_count,
            &rule.last_outcome,
            policy,
        );

        LearnedRouteDiagnostics {
            rule: rule.rule.clone(),
            effective_confidence: effective_confidence(
                rule.rule.confidence,
                rule.accept_count,
                rule.escalation_count,
                rule.failure_count,
                staleness_penalty,
                outcome_trend_adjustment,
            ),
            age_days,
            staleness_penalty,
            adaptive_decay_multiplier,
            outcome_trend_adjustment,
            accept_rate,
            escalation_rate,
            failure_rate,
            stability_score,
            stale: is_stale(age_days, policy),
            accept_count: rule.accept_count,
            escalation_count: rule.escalation_count,
            failure_count: rule.failure_count,
            last_outcome: rule.last_outcome.clone(),
        }
    }

    fn intent_key(intent: &TaskIntent) -> String {
        serde_json::to_string(intent).unwrap_or_else(|_| "\"Unknown\"".into())
    }

    fn rule_is_eligible(
        diagnostic: &LearnedRouteDiagnostics,
        policy: &LearnedRoutingConfig,
    ) -> bool {
        !diagnostic.stale
            && diagnostic.effective_confidence >= policy.min_effective_confidence
            && diagnostic.rule.sample_count >= policy.min_samples
    }
}

fn effective_confidence(
    confidence: f64,
    accept_count: u32,
    escalation_count: u32,
    failure_count: u32,
    staleness_penalty: f64,
    outcome_trend_adjustment: f64,
) -> f64 {
    let total = accept_count + escalation_count + failure_count;
    if total == 0 {
        return (confidence - staleness_penalty + outcome_trend_adjustment).clamp(0.0, 1.0);
    }

    let (accept_rate, escalation_rate, failure_rate) =
        routing_rates(accept_count, escalation_count, failure_count);
    let stability = stability_score(accept_rate, escalation_rate, failure_rate);
    (confidence + ((accept_rate - 0.5).max(0.0) * 0.08)
        - (0.16 * escalation_rate)
        - (0.26 * failure_rate)
        + ((stability - 0.5).max(0.0) * 0.05)
        - staleness_penalty
        + outcome_trend_adjustment)
        .clamp(0.0, 1.0)
}

fn routing_rates(accept_count: u32, escalation_count: u32, failure_count: u32) -> (f64, f64, f64) {
    let total = accept_count + escalation_count + failure_count;
    if total == 0 {
        return (0.0, 0.0, 0.0);
    }

    let total = total as f64;
    (
        accept_count as f64 / total,
        escalation_count as f64 / total,
        failure_count as f64 / total,
    )
}

fn age_days(last_updated: chrono::DateTime<Utc>) -> u32 {
    Utc::now()
        .signed_duration_since(last_updated)
        .num_days()
        .max(0) as u32
}

fn staleness_penalty(age_days: u32, policy: &LearnedRoutingConfig) -> f64 {
    let decay_days = age_days.saturating_sub(policy.decay_after_days);
    decay_days as f64 * policy.staleness_penalty_per_day
}

fn adaptive_decay_multiplier(
    age_days: u32,
    accept_count: u32,
    escalation_count: u32,
    failure_count: u32,
    last_outcome: &str,
    policy: &LearnedRoutingConfig,
) -> f64 {
    let total_samples = accept_count + escalation_count + failure_count;
    let recency = recency_factor(age_days, policy);
    let maturity = sample_maturity(total_samples, policy);
    let (_, escalation_rate, failure_rate) =
        routing_rates(accept_count, escalation_count, failure_count);
    let instability = (0.38 * escalation_rate + 0.72 * failure_rate) * recency;
    let immaturity_penalty = (1.0 - maturity) * 0.14;
    let outcome_factor = -recent_outcome_bias(last_outcome) * 0.18 * recency;

    (1.0 + instability + immaturity_penalty + outcome_factor).max(0.72)
}

fn outcome_trend_adjustment(
    age_days: u32,
    accept_count: u32,
    escalation_count: u32,
    failure_count: u32,
    last_outcome: &str,
    policy: &LearnedRoutingConfig,
) -> f64 {
    let total_samples = accept_count + escalation_count + failure_count;
    let recency = recency_factor(age_days, policy);
    let maturity = sample_maturity(total_samples, policy);
    let (accept_rate, escalation_rate, failure_rate) =
        routing_rates(accept_count, escalation_count, failure_count);
    let stability = stability_score(accept_rate, escalation_rate, failure_rate);
    let net_success = accept_rate - failure_rate - (0.65 * escalation_rate);
    let baseline_shift = (net_success - 0.45) * 0.11 * maturity * recency;
    let recent_outcome = recent_outcome_bias(last_outcome) * 0.045 * recency;
    let stability_component = (stability - 0.5) * 0.05 * maturity;

    (baseline_shift + recent_outcome + stability_component).clamp(-0.18, 0.14)
}

fn recency_factor(age_days: u32, policy: &LearnedRoutingConfig) -> f64 {
    let horizon = policy
        .max_rule_age_days
        .max(policy.decay_after_days.saturating_add(7))
        .max(1);
    (1.0 - (age_days as f64 / horizon as f64)).clamp(0.0, 1.0)
}

fn sample_maturity(total_samples: u32, policy: &LearnedRoutingConfig) -> f64 {
    let target = policy.min_samples.max(3) as f64;
    (total_samples as f64 / target).clamp(0.35, 1.0)
}

fn stability_score(accept_rate: f64, escalation_rate: f64, failure_rate: f64) -> f64 {
    (accept_rate - (0.35 * escalation_rate) - (0.75 * failure_rate)).clamp(0.0, 1.0)
}

fn recent_outcome_bias(last_outcome: &str) -> f64 {
    match last_outcome {
        "accepted_recovered" => 0.75,
        "accepted_verified" => 0.65,
        "accepted" => 0.55,
        "accepted_unverified" => 0.20,
        "escalated" => -0.45,
        "failed" => -0.75,
        _ => 0.0,
    }
}

fn complexity_supported(max_complexity: Option<TaskComplexity>, requested: TaskComplexity) -> bool {
    max_complexity.is_none_or(|max_complexity| requested <= max_complexity)
}

fn learned_route_candidate_score(
    diagnostic: &LearnedRouteDiagnostics,
    classification: &TaskClassification,
) -> f64 {
    let domain_bonus = diagnostic
        .rule
        .domain_filter
        .as_ref()
        .map(|domain| {
            if classification.domain_tags.iter().any(|tag| tag == domain) {
                0.05
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    let complexity_bonus = diagnostic
        .rule
        .max_complexity
        .map(|max_complexity| {
            let headroom = complexity_headroom(max_complexity, classification.complexity);
            0.03 - (headroom * 0.01)
        })
        .unwrap_or(0.005);

    diagnostic.effective_confidence
        + domain_bonus
        + complexity_bonus
        + (diagnostic.stability_score * 0.04)
        + (diagnostic.accept_rate * 0.02)
}

fn complexity_headroom(max_complexity: TaskComplexity, requested: TaskComplexity) -> f64 {
    complexity_rank(max_complexity).saturating_sub(complexity_rank(requested)) as f64
}

fn complexity_rank(complexity: TaskComplexity) -> u8 {
    match complexity {
        TaskComplexity::Trivial => 0,
        TaskComplexity::Simple => 1,
        TaskComplexity::Moderate => 2,
        TaskComplexity::Complex => 3,
        TaskComplexity::Expert => 4,
    }
}

fn is_stale(age_days: u32, policy: &LearnedRoutingConfig) -> bool {
    policy.max_rule_age_days > 0 && age_days > policy.max_rule_age_days
}

fn update_bucket(
    bucket: &mut BucketAccumulator,
    diagnostic: &LearnedRouteDiagnostics,
    eligible: bool,
) {
    bucket.total_rules += 1;
    bucket.eligible_rules += usize::from(eligible);
    bucket.accepted_samples += diagnostic.accept_count;
    bucket.escalated_samples += diagnostic.escalation_count;
    bucket.failed_samples += diagnostic.failure_count;
    bucket.total_effective_confidence += diagnostic.effective_confidence;
    bucket.total_age_days += u64::from(diagnostic.age_days);
}

fn buckets_to_summary(
    buckets: BTreeMap<String, BucketAccumulator>,
) -> Vec<LearnedRouteBucketSummary> {
    let mut summaries = buckets
        .into_iter()
        .map(|(label, bucket)| {
            let divisor = bucket.total_rules.max(1) as f64;
            LearnedRouteBucketSummary {
                label,
                total_rules: bucket.total_rules,
                eligible_rules: bucket.eligible_rules,
                accepted_samples: bucket.accepted_samples,
                escalated_samples: bucket.escalated_samples,
                failed_samples: bucket.failed_samples,
                avg_effective_confidence: bucket.total_effective_confidence / divisor,
                avg_age_days: bucket.total_age_days as f64 / divisor,
            }
        })
        .collect::<Vec<_>>();

    summaries.sort_by(|left, right| {
        right
            .total_rules
            .cmp(&left.total_rules)
            .then_with(|| {
                right
                    .avg_effective_confidence
                    .partial_cmp(&left.avg_effective_confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.label.cmp(&right.label))
    });

    summaries
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
            user_id: Some(ngenorca_core::types::UserId("test-user".into())),
            channel: Some("web".into()),
            latency_ms: 100,
            total_tokens: 200,
            correction: CorrectionRecord {
                tool_rounds: 1,
                had_failures: false,
                had_blocked_calls: false,
                verification_attempted: true,
                grounded: true,
                remediation_attempted: false,
                remediation_succeeded: false,
                post_synthesis_verification_attempted: false,
                post_synthesis_drift_corrected: false,
            },
            synthesis: SynthesisRecord {
                attempted: false,
                succeeded: false,
                contradiction_score: 0.0,
                conflicting_branches: 0,
            },
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn empty_lookup_returns_none() {
        let router = make_router();
        assert!(router.lookup(&TaskIntent::Coding, &[]).unwrap().is_none());
    }

    #[test]
    fn grounded_accepts_are_recorded_as_verified_outcomes() {
        let router = make_router();
        let record = make_record(
            TaskIntent::Coding,
            "coder",
            QualityVerdict::Accept { score: Some(0.92) },
            false,
        );

        router.ingest(&record).unwrap();
        let diagnostic = router
            .lookup_diagnostic(&TaskIntent::Coding, &[])
            .unwrap()
            .expect("diagnostic should exist");

        assert_eq!(diagnostic.last_outcome, "accepted_verified");
        assert!(diagnostic.outcome_trend_adjustment > 0.0);
        assert!(diagnostic.accept_rate > 0.0);
        assert!(diagnostic.stability_score > 0.0);
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
            .diagnostics_with_policy(
                &LearnedRoutingConfig {
                    min_effective_confidence: 0.0,
                    ..LearnedRoutingConfig::default()
                },
                true,
            )
            .unwrap()[0]
            .rule
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
            .diagnostics_with_policy(
                &LearnedRoutingConfig {
                    min_effective_confidence: 0.0,
                    ..LearnedRoutingConfig::default()
                },
                true,
            )
            .unwrap()[0]
            .rule
            .confidence;
        assert!(
            after < before,
            "Confidence should decrease: {} -> {}",
            before,
            after
        );
    }

    #[test]
    fn diagnostics_track_penalties() {
        let router = make_router();
        router
            .ingest(&make_record(
                TaskIntent::Analysis,
                "analyzer",
                QualityVerdict::Accept { score: Some(0.8) },
                false,
            ))
            .unwrap();
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

        let diagnostic = router
            .lookup_diagnostic_with_policy(
                &TaskIntent::Analysis,
                &[],
                &LearnedRoutingConfig {
                    min_effective_confidence: 0.0,
                    ..LearnedRoutingConfig::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(diagnostic.accept_count, 1);
        assert_eq!(diagnostic.escalation_count, 1);
        assert_eq!(diagnostic.failure_count, 0);
        assert_eq!(diagnostic.last_outcome, "escalated");
        assert!(diagnostic.outcome_trend_adjustment < 0.0);
        assert!(diagnostic.effective_confidence < diagnostic.rule.confidence);
    }

    #[test]
    fn lookup_prefers_less_penalized_rule() {
        let router = make_router();
        router
            .ingest(&make_record(
                TaskIntent::Coding,
                "fragile-coder",
                QualityVerdict::Accept { score: Some(0.8) },
                false,
            ))
            .unwrap();
        for _ in 0..3 {
            router
                .ingest(&make_record(
                    TaskIntent::Coding,
                    "fragile-coder",
                    QualityVerdict::Escalate {
                        reason: "needs help".into(),
                        escalate_to: None,
                    },
                    true,
                ))
                .unwrap();
        }

        router
            .ingest(&make_record(
                TaskIntent::Coding,
                "stable-coder",
                QualityVerdict::Accept { score: Some(0.85) },
                false,
            ))
            .unwrap();

        let rule = router.lookup(&TaskIntent::Coding, &[]).unwrap().unwrap();
        assert_eq!(rule.target_agent, "stable-coder");
    }

    #[test]
    fn diagnostics_respect_policy_thresholds() {
        let router = make_router();
        router
            .ingest(&make_record(
                TaskIntent::Coding,
                "coder",
                QualityVerdict::Accept { score: Some(0.8) },
                false,
            ))
            .unwrap();

        let hidden = router
            .diagnostics_with_policy(
                &LearnedRoutingConfig {
                    min_samples: 2,
                    ..LearnedRoutingConfig::default()
                },
                false,
            )
            .unwrap();
        assert!(hidden.is_empty());

        let shown = router
            .diagnostics_with_policy(
                &LearnedRoutingConfig {
                    min_samples: 2,
                    ..LearnedRoutingConfig::default()
                },
                true,
            )
            .unwrap();
        assert_eq!(shown.len(), 1);
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
    fn task_lookup_prefers_tighter_complexity_fit_when_confidence_is_close() {
        let router = make_router();

        let mut moderate = make_record(
            TaskIntent::Planning,
            "moderate-planner",
            QualityVerdict::Accept { score: Some(0.86) },
            false,
        );
        moderate.classification.complexity = TaskComplexity::Moderate;
        router.ingest(&moderate).unwrap();

        let mut expert = make_record(
            TaskIntent::Planning,
            "expert-planner",
            QualityVerdict::Accept { score: Some(0.88) },
            false,
        );
        expert.classification.complexity = TaskComplexity::Expert;
        router.ingest(&expert).unwrap();

        let classification = TaskClassification {
            intent: TaskIntent::Planning,
            complexity: TaskComplexity::Moderate,
            confidence: 0.9,
            method: ngenorca_core::orchestration::ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: Some("en".into()),
        };

        let diagnostic = router
            .lookup_for_task_with_policy(&classification, &LearnedRoutingConfig::default())
            .unwrap()
            .unwrap();

        assert_eq!(diagnostic.rule.target_agent, "moderate-planner");
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

    #[test]
    fn stale_rules_are_hidden_from_runtime_lookup() {
        let router = make_router();
        let mut record = make_record(
            TaskIntent::Coding,
            "coder",
            QualityVerdict::Accept { score: Some(0.9) },
            false,
        );
        record.timestamp = Utc::now() - chrono::Duration::days(120);
        router.ingest(&record).unwrap();

        let lookup = router.lookup(&TaskIntent::Coding, &[]).unwrap();
        assert!(lookup.is_none());

        let diagnostics = router
            .diagnostics_with_policy(&LearnedRoutingConfig::default(), true)
            .unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].stale);
        assert!(diagnostics[0].age_days >= 120);
    }

    #[test]
    fn age_penalty_reduces_effective_confidence() {
        let router = make_router();
        let mut record = make_record(
            TaskIntent::Analysis,
            "analyst",
            QualityVerdict::Accept { score: Some(0.9) },
            false,
        );
        record.timestamp = Utc::now() - chrono::Duration::days(30);
        router.ingest(&record).unwrap();

        let diagnostics = router
            .diagnostics_with_policy(
                &LearnedRoutingConfig {
                    min_effective_confidence: 0.0,
                    ..LearnedRoutingConfig::default()
                },
                true,
            )
            .unwrap();

        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].staleness_penalty > 0.0);
        assert!(diagnostics[0].adaptive_decay_multiplier > 0.0);
        assert!(diagnostics[0].effective_confidence < diagnostics[0].rule.confidence);
    }

    #[test]
    fn recent_failure_penalizes_more_than_older_failure() {
        let recent = effective_confidence(
            0.8,
            4,
            0,
            1,
            staleness_penalty(1, &LearnedRoutingConfig::default())
                * adaptive_decay_multiplier(1, 4, 0, 1, "failed", &LearnedRoutingConfig::default()),
            outcome_trend_adjustment(1, 4, 0, 1, "failed", &LearnedRoutingConfig::default()),
        );
        let older = effective_confidence(
            0.8,
            4,
            0,
            1,
            staleness_penalty(10, &LearnedRoutingConfig::default())
                * adaptive_decay_multiplier(
                    10,
                    4,
                    0,
                    1,
                    "failed",
                    &LearnedRoutingConfig::default(),
                ),
            outcome_trend_adjustment(10, 4, 0, 1, "failed", &LearnedRoutingConfig::default()),
        );

        assert!(recent < older);
    }

    #[test]
    fn recent_accept_gets_small_positive_trend_adjustment() {
        let router = make_router();
        router
            .ingest(&make_record(
                TaskIntent::Planning,
                "planner",
                QualityVerdict::Accept { score: Some(0.88) },
                false,
            ))
            .unwrap();

        let diagnostic = router
            .lookup_diagnostic_with_policy(
                &TaskIntent::Planning,
                &[],
                &LearnedRoutingConfig {
                    min_effective_confidence: 0.0,
                    ..LearnedRoutingConfig::default()
                },
            )
            .unwrap()
            .unwrap();

        assert!(diagnostic.outcome_trend_adjustment > 0.0);
        assert!(diagnostic.adaptive_decay_multiplier < 1.0);
    }

    #[test]
    fn history_summary_groups_by_intent_agent_and_domain() {
        let router = make_router();

        let mut rust_record = make_record(
            TaskIntent::Coding,
            "coder",
            QualityVerdict::Accept { score: Some(0.9) },
            false,
        );
        rust_record.classification.domain_tags = vec!["rust".into()];
        router.ingest(&rust_record).unwrap();

        let mut docs_record = make_record(
            TaskIntent::Coding,
            "coder",
            QualityVerdict::Accept { score: Some(0.82) },
            false,
        );
        docs_record.classification.domain_tags = vec!["docs".into()];
        router.ingest(&docs_record).unwrap();

        let mut docs_escalated = make_record(
            TaskIntent::Coding,
            "coder",
            QualityVerdict::Escalate {
                reason: "needs help".into(),
                escalate_to: None,
            },
            true,
        );
        docs_escalated.classification.domain_tags = vec!["docs".into()];
        router.ingest(&docs_escalated).unwrap();

        router
            .ingest(&make_record(
                TaskIntent::Analysis,
                "analyst",
                QualityVerdict::Accept { score: Some(0.85) },
                false,
            ))
            .unwrap();

        let summary = router
            .history_summary_with_policy(&LearnedRoutingConfig::default(), true)
            .unwrap();

        assert_eq!(summary.total_rules, 3);
        assert!(summary.intents.iter().any(|bucket| {
            bucket.label == "Coding"
                && bucket.total_rules == 2
                && bucket.accepted_samples == 2
                && bucket.escalated_samples == 1
        }));
        assert!(summary.agents.iter().any(|bucket| {
            bucket.label == "coder"
                && bucket.total_rules == 2
                && bucket.accepted_samples == 2
                && bucket.escalated_samples == 1
        }));
        assert!(summary.domains.iter().any(|bucket| bucket.label == "rust"));
        assert!(summary.domains.iter().any(|bucket| bucket.label == "docs"));
        assert!(
            summary
                .domains
                .iter()
                .any(|bucket| bucket.label == "(none)")
        );
    }
}
