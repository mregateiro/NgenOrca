//! Event types for the durable event log.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{EventId, SessionId, UserId};

/// An event in the durable event log. Everything that happens in NgenOrca
/// is persisted as an event — messages, state changes, tool calls, identity
/// changes, memory operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique, time-ordered event ID (ULID).
    pub id: EventId,

    /// When this event occurred.
    pub timestamp: DateTime<Utc>,

    /// The session this event belongs to (if applicable).
    pub session_id: Option<SessionId>,

    /// The user this event relates to (if known).
    pub user_id: Option<UserId>,

    /// Event payload.
    pub payload: EventPayload,
}

/// The different kinds of events that can be logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventPayload {
    /// A message was sent or received.
    Message(crate::message::Message),

    /// A session was created.
    SessionCreated {
        session_id: SessionId,
        user_id: Option<UserId>,
    },

    /// A session was ended.
    SessionEnded { session_id: SessionId },

    /// A plugin was loaded.
    PluginLoaded {
        plugin_id: crate::PluginId,
        version: String,
    },

    /// A plugin was unloaded.
    PluginUnloaded { plugin_id: crate::PluginId },

    /// Identity was verified or changed.
    IdentityChange {
        user_id: UserId,
        change: IdentityChangeKind,
    },

    /// Memory was updated (tier 2 or 3).
    MemoryUpdate {
        user_id: UserId,
        tier: MemoryTier,
        operation: MemoryOperation,
    },

    /// Agent tool execution.
    ToolExecution {
        tool_name: String,
        session_id: SessionId,
        started_at: DateTime<Utc>,
        duration_ms: Option<u64>,
        success: Option<bool>,
    },

    /// System lifecycle event.
    SystemLifecycle(LifecycleEvent),

    /// An orchestration cycle completed (for analytics / learned routing).
    OrchestrationCompleted(crate::orchestration::OrchestrationRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IdentityChangeKind {
    DevicePaired { device_id: crate::DeviceId },
    DeviceRevoked { device_id: crate::DeviceId },
    ChannelLinked { channel: String, handle: String },
    ChannelUnlinked { channel: String },
    TrustElevated { from: crate::TrustLevel, to: crate::TrustLevel },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryTier {
    Working,
    Episodic,
    Semantic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemoryOperation {
    Store { key: String },
    Retrieve { query: String, results: usize },
    Consolidate { entries_processed: usize },
    Prune { entries_removed: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LifecycleEvent {
    GatewayStarted,
    GatewayShutdown,
    PluginCrashed { plugin_id: crate::PluginId, reason: String },
    AdapterConnected { channel: String },
    AdapterDisconnected { channel: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EventId, SessionId, UserId};

    fn sample_event() -> Event {
        Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("alice".into())),
            payload: EventPayload::SessionCreated {
                session_id: SessionId::new(),
                user_id: Some(UserId("alice".into())),
            },
        }
    }

    #[test]
    fn event_serde_roundtrip_session_created() {
        let e = sample_event();
        let json = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(e.id, back.id);
        assert_eq!(e.session_id, back.session_id);
        assert_eq!(e.user_id, back.user_id);
    }

    #[test]
    fn event_payload_message_variant() {
        let msg = crate::message::Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId("bob".into())),
            trust: crate::types::TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: crate::types::ChannelId("ch-1".into()),
            channel_kind: crate::types::ChannelKind::Telegram,
            direction: crate::message::Direction::Inbound,
            content: crate::message::Content::Text("hello".into()),
            metadata: serde_json::Value::Null,
        };
        let payload = EventPayload::Message(msg);
        let json = serde_json::to_string(&payload).unwrap();
        let back: EventPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, EventPayload::Message(_)));
    }

    #[test]
    fn event_payload_all_simple_variants_serialize() {
        let variants: Vec<EventPayload> = vec![
            EventPayload::SessionCreated {
                session_id: SessionId::new(),
                user_id: None,
            },
            EventPayload::SessionEnded {
                session_id: SessionId::new(),
            },
            EventPayload::PluginLoaded {
                plugin_id: crate::types::PluginId("p1".into()),
                version: "0.1.0".into(),
            },
            EventPayload::PluginUnloaded {
                plugin_id: crate::types::PluginId("p1".into()),
            },
        ];
        for v in &variants {
            let json = serde_json::to_string(v).unwrap();
            assert!(!json.is_empty());
        }
    }

    #[test]
    fn memory_tier_serde_roundtrip() {
        let tiers = vec![MemoryTier::Working, MemoryTier::Episodic, MemoryTier::Semantic];
        for t in &tiers {
            let json = serde_json::to_string(t).unwrap();
            let back: MemoryTier = serde_json::from_str(&json).unwrap();
            assert_eq!(*t, back);
        }
    }

    #[test]
    fn identity_change_kind_serde_roundtrip() {
        let kinds = vec![
            IdentityChangeKind::DevicePaired {
                device_id: crate::DeviceId("dev-1".into()),
            },
            IdentityChangeKind::DeviceRevoked {
                device_id: crate::DeviceId("dev-1".into()),
            },
            IdentityChangeKind::ChannelLinked {
                channel: "telegram".into(),
                handle: "@alice".into(),
            },
            IdentityChangeKind::ChannelUnlinked {
                channel: "telegram".into(),
            },
            IdentityChangeKind::TrustElevated {
                from: crate::TrustLevel::Channel,
                to: crate::TrustLevel::Hardware,
            },
        ];
        for k in &kinds {
            let json = serde_json::to_string(k).unwrap();
            let back: IdentityChangeKind = serde_json::from_str(&json).unwrap();
            // Verify roundtrip succeeds (no PartialEq on IdentityChangeKind)
            let _ = format!("{:?}", back);
        }
    }

    #[test]
    fn lifecycle_event_serde_roundtrip() {
        let events = vec![
            LifecycleEvent::GatewayStarted,
            LifecycleEvent::GatewayShutdown,
            LifecycleEvent::PluginCrashed {
                plugin_id: crate::PluginId("test".into()),
                reason: "panic".into(),
            },
            LifecycleEvent::AdapterConnected {
                channel: "telegram".into(),
            },
        ];
        for le in &events {
            let json = serde_json::to_string(le).unwrap();
            let back: LifecycleEvent = serde_json::from_str(&json).unwrap();
            // Just verify roundtrip doesn't panic
            let _ = format!("{:?}", back);
        }
    }

    #[test]
    fn event_payload_orchestration_completed_serde() {
        use crate::orchestration::*;
        let record = OrchestrationRecord {
            classification: TaskClassification {
                intent: TaskIntent::Conversation,
                complexity: TaskComplexity::Simple,
                confidence: 0.9,
                method: ClassificationMethod::RuleBased,
                domain_tags: vec![],
                language: Some("en".into()),
            },
            routing: RoutingDecision {
                target: SubAgentId { name: "local".into(), model: "phi-3".into() },
                reason: "rules".into(),
                system_prompt: String::new(),
                max_tokens: None,
                temperature: None,
                from_memory: false,
            },
            quality: QualityVerdict::Accept { score: Some(0.85) },
            quality_method: QualityMethod::Heuristic,
            escalated: false,
            latency_ms: 120,
            total_tokens: 256,
            timestamp: chrono::Utc::now(),
        };
        let payload = EventPayload::OrchestrationCompleted(record);
        let json = serde_json::to_string(&payload).unwrap();
        let back: EventPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, EventPayload::OrchestrationCompleted(_)));
    }

    #[test]
    fn event_without_session_or_user() {
        let e = Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: None,
            user_id: None,
            payload: EventPayload::SystemLifecycle(LifecycleEvent::GatewayStarted),
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert!(back.session_id.is_none());
        assert!(back.user_id.is_none());
    }
}
