//! Tier 1: Working Memory — active conversation context window.

use ngenorca_core::types::SessionId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// A message in working memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingMessage {
    /// Role: "user", "assistant", "system", "tool".
    pub role: String,
    /// Text content.
    pub content: String,
    /// Timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Token count estimate.
    pub estimated_tokens: usize,
}

/// Working memory manages per-session context windows.
pub struct WorkingMemory {
    sessions: RwLock<HashMap<SessionId, SessionContext>>,
}

struct SessionContext {
    messages: Vec<WorkingMessage>,
    total_tokens: usize,
    max_tokens: usize,
}

impl WorkingMemory {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Add a message to a session's working memory.
    pub fn push(&self, session_id: &SessionId, message: WorkingMessage) {
        let mut sessions = self.sessions.write().unwrap();
        let ctx = sessions.entry(session_id.clone()).or_insert(SessionContext {
            messages: Vec::new(),
            total_tokens: 0,
            max_tokens: 128_000, // Default context window
        });

        ctx.total_tokens += message.estimated_tokens;
        ctx.messages.push(message);

        // If over budget, evict oldest non-system messages.
        while ctx.total_tokens > ctx.max_tokens && ctx.messages.len() > 2 {
            // Find first non-system message to evict.
            if let Some(idx) = ctx.messages.iter().position(|m| m.role != "system") {
                ctx.total_tokens -= ctx.messages[idx].estimated_tokens;
                ctx.messages.remove(idx);
            } else {
                break;
            }
        }
    }

    /// Get all messages for a session.
    pub fn get_session(&self, session_id: &SessionId) -> Vec<WorkingMessage> {
        let sessions = self.sessions.read().unwrap();
        sessions
            .get(session_id)
            .map(|ctx| ctx.messages.clone())
            .unwrap_or_default()
    }

    /// Clear a session's working memory.
    pub fn clear_session(&self, session_id: &SessionId) {
        let mut sessions = self.sessions.write().unwrap();
        sessions.remove(session_id);
    }

    /// Set the max token budget for a session (adapts to model context window).
    pub fn set_max_tokens(&self, session_id: &SessionId, max_tokens: usize) {
        let mut sessions = self.sessions.write().unwrap();
        if let Some(ctx) = sessions.get_mut(session_id) {
            ctx.max_tokens = max_tokens;
        }
    }

    /// Get approximate token usage for a session.
    pub fn token_usage(&self, session_id: &SessionId) -> usize {
        let sessions = self.sessions.read().unwrap();
        sessions
            .get(session_id)
            .map(|ctx| ctx.total_tokens)
            .unwrap_or(0)
    }
}

impl Default for WorkingMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::types::SessionId;

    fn msg(role: &str, content: &str, tokens: usize) -> WorkingMessage {
        WorkingMessage {
            role: role.into(),
            content: content.into(),
            timestamp: chrono::Utc::now(),
            estimated_tokens: tokens,
        }
    }

    #[test]
    fn new_is_empty() {
        let wm = WorkingMemory::new();
        let sid = SessionId::new();
        assert!(wm.get_session(&sid).is_empty());
        assert_eq!(wm.token_usage(&sid), 0);
    }

    #[test]
    fn push_and_get_session() {
        let wm = WorkingMemory::new();
        let sid = SessionId::new();
        wm.push(&sid, msg("user", "hello", 10));
        wm.push(&sid, msg("assistant", "hi", 5));

        let messages = wm.get_session(&sid);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn token_usage_tracks_correctly() {
        let wm = WorkingMemory::new();
        let sid = SessionId::new();
        wm.push(&sid, msg("user", "hello", 100));
        wm.push(&sid, msg("assistant", "response", 200));
        assert_eq!(wm.token_usage(&sid), 300);
    }

    #[test]
    fn clear_session_removes_all() {
        let wm = WorkingMemory::new();
        let sid = SessionId::new();
        wm.push(&sid, msg("user", "hello", 10));
        assert_eq!(wm.get_session(&sid).len(), 1);

        wm.clear_session(&sid);
        assert!(wm.get_session(&sid).is_empty());
        assert_eq!(wm.token_usage(&sid), 0);
    }

    #[test]
    fn sessions_are_isolated() {
        let wm = WorkingMemory::new();
        let sid1 = SessionId::new();
        let sid2 = SessionId::new();
        wm.push(&sid1, msg("user", "s1", 10));
        wm.push(&sid2, msg("user", "s2", 20));

        assert_eq!(wm.get_session(&sid1).len(), 1);
        assert_eq!(wm.get_session(&sid2).len(), 1);
        assert_eq!(wm.get_session(&sid1)[0].content, "s1");
        assert_eq!(wm.get_session(&sid2)[0].content, "s2");
    }

    #[test]
    fn eviction_removes_oldest_non_system_messages() {
        let wm = WorkingMemory::new();
        let sid = SessionId::new();
        wm.set_max_tokens(&sid, 50); // won't work — session not yet created

        // Create session with small budget
        wm.push(&sid, msg("system", "you are a bot", 10));
        wm.set_max_tokens(&sid, 30); // now session exists

        wm.push(&sid, msg("user", "first", 15));
        // Over budget: 10 + 15 = 25, still within 30
        wm.push(&sid, msg("user", "second", 15));
        // Now 10 + 15 + 15 = 40, over 30. Eviction should remove "first"

        let messages = wm.get_session(&sid);
        // system should be preserved
        assert!(messages.iter().any(|m| m.role == "system"));
        // token_usage should be <= 30
        assert!(wm.token_usage(&sid) <= 30);
    }

    #[test]
    fn default_is_new() {
        let wm = WorkingMemory::default();
        let sid = SessionId::new();
        assert!(wm.get_session(&sid).is_empty());
    }

    #[test]
    fn working_message_serde_roundtrip() {
        let m = msg("user", "test message", 42);
        let json = serde_json::to_string(&m).unwrap();
        let back: WorkingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, "user");
        assert_eq!(back.content, "test message");
        assert_eq!(back.estimated_tokens, 42);
    }
}
