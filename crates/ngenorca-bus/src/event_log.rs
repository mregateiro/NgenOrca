//! SQLite WAL-backed durable event log.

use chrono::{DateTime, Utc};
use ngenorca_core::event::Event;
use ngenorca_core::{Error, EventId, Result, SessionId};
use crate::EventQuery;
use rusqlite::{Connection, params};
use std::sync::Mutex;
use tracing::debug;

/// Durable event log backed by SQLite in WAL mode.
pub struct EventLog {
    conn: Mutex<Connection>,
}

impl EventLog {
    /// Open or create the event log database.
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path).map_err(|e| Error::Database(e.to_string()))?;

        // Enable WAL mode for concurrent reads + single writer.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| Error::Database(e.to_string()))?;

        // Sync mode = NORMAL for WAL (safe + fast).
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| Error::Database(e.to_string()))?;

        // Create the events table.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id          TEXT PRIMARY KEY,
                timestamp   TEXT NOT NULL,
                session_id  TEXT,
                user_id     TEXT,
                payload     TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_session
                ON events(session_id) WHERE session_id IS NOT NULL;

            CREATE INDEX IF NOT EXISTS idx_events_timestamp
                ON events(timestamp);

            CREATE INDEX IF NOT EXISTS idx_events_user
                ON events(user_id) WHERE user_id IS NOT NULL;",
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        debug!("Event log opened: {}", db_path);

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Append an event to the log.
    pub fn append(&self, event: &Event) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let payload = serde_json::to_string(&event.payload)?;
        let session_id = event.session_id.as_ref().map(|s| s.0.to_string());
        let user_id = event.user_id.as_ref().map(|u| u.0.clone());

        conn.execute(
            "INSERT INTO events (id, timestamp, session_id, user_id, payload)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.id.0.to_string(),
                event.timestamp.to_rfc3339(),
                session_id,
                user_id,
                payload,
            ],
        )
        .map_err(|e| Error::Database(e.to_string()))?;

        Ok(())
    }

    /// Query events, optionally filtered by session, with an optional limit.
    pub fn query(
        &self,
        session_id: Option<&SessionId>,
        limit: Option<usize>,
    ) -> Result<Vec<Event>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match session_id {
            Some(sid) => {
                let limit_val = limit.unwrap_or(1000) as i64;
                (
                    "SELECT id, timestamp, session_id, user_id, payload
                     FROM events WHERE session_id = ?1
                     ORDER BY id ASC LIMIT ?2"
                        .to_string(),
                    vec![Box::new(sid.0.to_string()), Box::new(limit_val)],
                )
            }
            None => {
                let limit_val = limit.unwrap_or(1000) as i64;
                (
                    "SELECT id, timestamp, session_id, user_id, payload
                     FROM events ORDER BY id ASC LIMIT ?1"
                        .to_string(),
                    vec![Box::new(limit_val)],
                )
            }
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| Error::Database(e.to_string()))?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), decode_event_row)
            .map_err(|e| Error::Database(e.to_string()))?;

        collect_events(rows)
    }

    /// Query events using durable filters for longer-horizon operator diagnostics.
    pub fn query_filtered(&self, query: &EventQuery) -> Result<Vec<Event>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let mut sql = String::from(
            "SELECT id, timestamp, session_id, user_id, payload FROM events WHERE 1=1",
        );
        let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();

        if let Some(session_id) = query.session_id.as_ref() {
            sql.push_str(" AND session_id = ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(rusqlite::types::Value::Text(session_id.0.to_string()));
        }
        if let Some(user_id) = query.user_id.as_ref() {
            sql.push_str(" AND user_id = ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(rusqlite::types::Value::Text(user_id.0.clone()));
        }
        if let Some(since) = query.since.as_ref() {
            sql.push_str(" AND timestamp >= ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(rusqlite::types::Value::Text(since.to_rfc3339()));
        }
        if let Some(until) = query.until.as_ref() {
            sql.push_str(" AND timestamp <= ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(rusqlite::types::Value::Text(until.to_rfc3339()));
        }

        sql.push_str(" ORDER BY timestamp ASC, id ASC");

        if let Some(limit) = query.limit {
            sql.push_str(" LIMIT ?");
            sql.push_str(&(params_vec.len() + 1).to_string());
            params_vec.push(rusqlite::types::Value::Integer(limit as i64));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| Error::Database(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec.iter()), decode_event_row)
            .map_err(|e| Error::Database(e.to_string()))?;

        collect_events(rows)
    }

    /// Query the most recent events, newest-first in storage and returned oldest-first for stable aggregation.
    pub fn query_recent(&self, limit: usize) -> Result<Vec<Event>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp, session_id, user_id, payload
                 FROM events ORDER BY id DESC LIMIT ?1",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![limit as i64], decode_event_row)
            .map_err(|e| Error::Database(e.to_string()))?;

        let mut events = collect_events(rows)?;
        events.reverse();
        Ok(events)
    }

    /// Query events after a specific event ID (for catch-up replay).
    pub fn query_after(&self, after_id: &EventId) -> Result<Vec<Event>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp, session_id, user_id, payload
                 FROM events WHERE id > ?1
                 ORDER BY id ASC",
            )
            .map_err(|e| Error::Database(e.to_string()))?;

        let rows = stmt
            .query_map(params![after_id.0.to_string()], decode_event_row)
            .map_err(|e| Error::Database(e.to_string()))?;

        collect_events(rows)
    }

    /// Get total event count.
    pub fn count(&self) -> Result<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(count as u64)
    }

    /// Delete events older than the given timestamp.
    ///
    /// Returns the number of deleted events.
    pub fn prune_before(&self, older_than: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| Error::Database(e.to_string()))?;
        let cutoff = older_than.to_rfc3339();
        let deleted = conn
            .execute("DELETE FROM events WHERE timestamp < ?1", [&cutoff])
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(deleted)
    }
}

type EventRow = (String, String, Option<String>, Option<String>, String);

fn decode_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    let id: String = row.get(0)?;
    let timestamp: String = row.get(1)?;
    let session_id: Option<String> = row.get(2)?;
    let user_id: Option<String> = row.get(3)?;
    let payload: String = row.get(4)?;
    Ok((id, timestamp, session_id, user_id, payload))
}

fn collect_events<F>(rows: rusqlite::MappedRows<'_, F>) -> Result<Vec<Event>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<EventRow>,
{
    let mut events = Vec::new();
    for row in rows {
        let (id, timestamp, session_id, user_id, payload) =
            row.map_err(|e| Error::Database(e.to_string()))?;

        let event = Event {
            id: EventId(
                id.parse()
                    .map_err(|e: ulid::DecodeError| Error::Database(e.to_string()))?,
            ),
            timestamp: DateTime::parse_from_rfc3339(&timestamp)
                .map_err(|e| Error::Database(e.to_string()))?
                .with_timezone(&Utc),
            session_id: session_id
                .map(|s| SessionId(uuid::Uuid::parse_str(&s).unwrap_or_default())),
            user_id: user_id.map(ngenorca_core::UserId),
            payload: serde_json::from_str(&payload)
                .map_err(|e| Error::Database(e.to_string()))?,
        };
        events.push(event);
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::event::{Event, EventPayload};
    use ngenorca_core::types::{EventId, SessionId, UserId};

    fn sample_event(session_id: Option<SessionId>, user_id: Option<UserId>) -> Event {
        Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id,
            user_id,
            payload: EventPayload::SessionCreated {
                session_id: SessionId::new(),
                user_id: None,
            },
        }
    }

    #[test]
    fn create_event_log_in_memory() {
        let log = EventLog::new(":memory:").unwrap();
        assert_eq!(log.count().unwrap(), 0);
    }

    #[test]
    fn append_and_count() {
        let log = EventLog::new(":memory:").unwrap();
        let event = sample_event(None, None);
        log.append(&event).unwrap();
        assert_eq!(log.count().unwrap(), 1);

        let event2 = sample_event(None, None);
        log.append(&event2).unwrap();
        assert_eq!(log.count().unwrap(), 2);
    }

    #[test]
    fn query_all_events() {
        let log = EventLog::new(":memory:").unwrap();
        for _ in 0..5 {
            log.append(&sample_event(None, None)).unwrap();
        }
        let events = log.query(None, None).unwrap();
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn query_with_limit() {
        let log = EventLog::new(":memory:").unwrap();
        for _ in 0..10 {
            log.append(&sample_event(None, None)).unwrap();
        }
        let events = log.query(None, Some(3)).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn query_by_session() {
        let log = EventLog::new(":memory:").unwrap();
        let target_session = SessionId::new();
        let other_session = SessionId::new();

        for _ in 0..3 {
            log.append(&sample_event(Some(target_session.clone()), None))
                .unwrap();
        }
        for _ in 0..2 {
            log.append(&sample_event(Some(other_session.clone()), None))
                .unwrap();
        }

        let events = log.query(Some(&target_session), None).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn query_after_returns_later_events() {
        let log = EventLog::new(":memory:").unwrap();

        let first = sample_event(None, None);
        let first_id = first.id.clone();
        log.append(&first).unwrap();

        // Small delay to ensure ULID ordering
        std::thread::sleep(std::time::Duration::from_millis(2));

        let second = sample_event(None, None);
        log.append(&second).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));

        let third = sample_event(None, None);
        log.append(&third).unwrap();

        let after = log.query_after(&first_id).unwrap();
        assert_eq!(after.len(), 2);
    }

    #[test]
    fn event_roundtrip_preserves_data() {
        let log = EventLog::new(":memory:").unwrap();
        let sid = SessionId::new();
        let uid = UserId("alice".to_string());
        let event = sample_event(Some(sid.clone()), Some(uid.clone()));
        let original_id = event.id.clone();
        log.append(&event).unwrap();

        let events = log.query(None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, original_id);
        assert_eq!(events[0].session_id, Some(sid));
        assert_eq!(events[0].user_id, Some(uid));
    }

    #[test]
    fn query_filtered_respects_user_and_time_window() {
        let log = EventLog::new(":memory:").unwrap();

        let older = Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(72),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("alice".into())),
            payload: EventPayload::SessionCreated {
                session_id: SessionId::new(),
                user_id: Some(UserId("alice".into())),
            },
        };
        log.append(&older).unwrap();

        let recent = Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("bob".into())),
            payload: EventPayload::SessionCreated {
                session_id: SessionId::new(),
                user_id: Some(UserId("bob".into())),
            },
        };
        let recent_id = recent.id.clone();
        log.append(&recent).unwrap();

        let filtered = log
            .query_filtered(&EventQuery {
                user_id: Some(UserId("bob".into())),
                since: Some(chrono::Utc::now() - chrono::Duration::hours(24)),
                limit: Some(10),
                ..EventQuery::default()
            })
            .unwrap();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, recent_id);
        assert_eq!(filtered[0].user_id, Some(UserId("bob".into())));
    }
}
