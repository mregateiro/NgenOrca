//! Session management types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{ChannelId, SessionId, UserId};

/// A conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session ID.
    pub id: SessionId,

    /// The user this session belongs to.
    pub user_id: Option<UserId>,

    /// Which channel originated this session.
    pub origin_channel: ChannelId,

    /// Current session state.
    pub state: SessionState,

    /// Model to use for this session.
    pub model: String,

    /// Thinking level for the agent.
    pub thinking_level: ThinkingLevel,

    /// When this session was created.
    pub created_at: DateTime<Utc>,

    /// When this session was last active.
    pub last_active: DateTime<Utc>,

    /// Number of messages in this session.
    pub message_count: u64,

    /// Total tokens used in this session.
    pub tokens_used: u64,
}

/// Session lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Session is active and accepting messages.
    Active,
    /// Session is paused (user idle).
    Idle,
    /// Session is being compacted (summarizing context).
    Compacting,
    /// Session has ended.
    Ended,
}

/// Agent thinking intensity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Max,
}

impl Default for ThinkingLevel {
    fn default() -> Self {
        Self::Medium
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SessionId, UserId};

    #[test]
    fn thinking_level_default_is_medium() {
        assert_eq!(ThinkingLevel::default(), ThinkingLevel::Medium);
    }

    #[test]
    fn thinking_level_serde_roundtrip() {
        let levels = vec![
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Max,
        ];
        for level in &levels {
            let json = serde_json::to_string(level).unwrap();
            let back: ThinkingLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(*level, back);
        }
    }

    #[test]
    fn session_state_serde_roundtrip() {
        let states = vec![
            SessionState::Active,
            SessionState::Idle,
            SessionState::Compacting,
            SessionState::Ended,
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let back: SessionState = serde_json::from_str(&json).unwrap();
            assert_eq!(*state, back);
        }
    }

    #[test]
    fn session_serde_roundtrip() {
        let session = Session {
            id: SessionId::new(),
            user_id: Some(UserId("alice".into())),
            origin_channel: crate::types::ChannelId("webchat".into()),
            state: SessionState::Active,
            model: "ollama/llama3".into(),
            thinking_level: ThinkingLevel::High,
            created_at: chrono::Utc::now(),
            last_active: chrono::Utc::now(),
            message_count: 5,
            tokens_used: 1024,
        };
        let json = serde_json::to_string(&session).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session.id, back.id);
        assert_eq!(session.user_id, back.user_id);
        assert_eq!(session.state, back.state);
        assert_eq!(session.thinking_level, back.thinking_level);
        assert_eq!(session.message_count, back.message_count);
    }
}
