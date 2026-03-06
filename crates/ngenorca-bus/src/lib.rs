//! # NgenOrca Bus
//!
//! Durable event bus combining an in-memory pub/sub broadcast with a
//! SQLite WAL-backed event log. Every event published to the bus is:
//!
//! 1. Persisted to the SQLite event log (durable)
//! 2. Broadcast to all subscribers (real-time)
//!
//! This ensures no events are lost on crash, and all history is replayable.

pub mod event_log;
pub mod subscriber;

use std::sync::Arc;

use ngenorca_core::event::Event;
use ngenorca_core::Result;
use tokio::sync::broadcast;
use tracing::{info, warn};

use event_log::EventLog;

/// Capacity of the in-memory broadcast channel.
const BROADCAST_CAPACITY: usize = 4096;

/// The central event bus. Clone-friendly (wraps Arc internals).
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<EventBusInner>,
}

struct EventBusInner {
    /// Durable event log (SQLite).
    log: EventLog,
    /// In-memory broadcast sender.
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new event bus backed by the given SQLite database path.
    pub async fn new(db_path: &str) -> Result<Self> {
        let log = EventLog::new(db_path)?;
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);

        info!(db_path, "Event bus initialized");

        Ok(Self {
            inner: Arc::new(EventBusInner { log, tx }),
        })
    }

    /// Publish an event: persist to the log, then broadcast to subscribers.
    pub async fn publish(&self, event: Event) -> Result<()> {
        // 1. Persist first (durability guarantee).
        self.inner.log.append(&event)?;

        // 2. Broadcast to in-memory subscribers.
        if let Err(e) = self.inner.tx.send(event) {
            warn!("No active subscribers for event: {}", e);
        }

        Ok(())
    }

    /// Subscribe to real-time events from the bus.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.tx.subscribe()
    }

    /// Replay events from the log, optionally filtering by session.
    pub fn replay(
        &self,
        session_id: Option<&ngenorca_core::SessionId>,
        limit: Option<usize>,
    ) -> Result<Vec<Event>> {
        self.inner.log.query(session_id, limit)
    }

    /// Replay events after a specific event ID (for catch-up after reconnection).
    pub fn replay_after(&self, after_id: &ngenorca_core::EventId) -> Result<Vec<Event>> {
        self.inner.log.query_after(after_id)
    }

    /// Get total event count.
    pub fn event_count(&self) -> Result<u64> {
        self.inner.log.count()
    }

    /// Access the underlying event log for maintenance operations (e.g., pruning).
    pub fn event_log(&self) -> &EventLog {
        &self.inner.log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::event::{Event, EventPayload};
    use ngenorca_core::types::{EventId, SessionId, UserId};

    fn sample_event() -> Event {
        Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("test".into())),
            payload: EventPayload::SessionCreated {
                session_id: SessionId::new(),
                user_id: None,
            },
        }
    }

    #[tokio::test]
    async fn bus_publish_and_count() {
        let bus = EventBus::new(":memory:").await.unwrap();
        assert_eq!(bus.event_count().unwrap(), 0);

        bus.publish(sample_event()).await.unwrap();
        assert_eq!(bus.event_count().unwrap(), 1);

        bus.publish(sample_event()).await.unwrap();
        assert_eq!(bus.event_count().unwrap(), 2);
    }

    #[tokio::test]
    async fn bus_subscribe_receives_events() {
        let bus = EventBus::new(":memory:").await.unwrap();
        let mut rx = bus.subscribe();

        let event = sample_event();
        let event_id = event.id.clone();
        bus.publish(event).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, event_id);
    }

    #[tokio::test]
    async fn bus_replay_events() {
        let bus = EventBus::new(":memory:").await.unwrap();
        for _ in 0..5 {
            bus.publish(sample_event()).await.unwrap();
        }

        let events = bus.replay(None, None).unwrap();
        assert_eq!(events.len(), 5);
    }

    #[tokio::test]
    async fn bus_replay_with_limit() {
        let bus = EventBus::new(":memory:").await.unwrap();
        for _ in 0..10 {
            bus.publish(sample_event()).await.unwrap();
        }

        let events = bus.replay(None, Some(3)).unwrap();
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn bus_replay_after() {
        let bus = EventBus::new(":memory:").await.unwrap();

        let first = sample_event();
        let first_id = first.id.clone();
        bus.publish(first).await.unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));
        bus.publish(sample_event()).await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        bus.publish(sample_event()).await.unwrap();

        let after = bus.replay_after(&first_id).unwrap();
        assert_eq!(after.len(), 2);
    }

    #[tokio::test]
    async fn bus_clone_shares_state() {
        let bus1 = EventBus::new(":memory:").await.unwrap();
        let bus2 = bus1.clone();

        bus1.publish(sample_event()).await.unwrap();
        assert_eq!(bus2.event_count().unwrap(), 1);
    }
}
