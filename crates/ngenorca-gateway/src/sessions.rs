//! Session management — tracks active conversation sessions.
//!
//! Sessions are the bridge between a user message and the agent pipeline.
//! Each session tracks the user, channel, model, token usage, and state.

use chrono::Utc;
use ngenorca_core::session::{Session, SessionState, ThinkingLevel};
use ngenorca_core::types::{ChannelId, SessionId, UserId};
use ngenorca_core::{Error, Result};
use std::collections::HashMap;
use std::sync::RwLock;
use tracing::{debug, info};

/// Manages active sessions.
pub struct SessionManager {
    sessions: RwLock<HashMap<SessionId, Session>>,
    /// Maps user_id + channel → active session for that combination.
    user_sessions: RwLock<HashMap<(String, String), SessionId>>,
    default_model: String,
    default_thinking_level: ThinkingLevel,
}

impl SessionManager {
    pub fn new(default_model: String, default_thinking_level: ThinkingLevel) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            user_sessions: RwLock::new(HashMap::new()),
            default_model,
            default_thinking_level,
        }
    }

    /// Get or create a session for a user+channel combination.
    /// If the user already has an active session on this channel, reuse it.
    pub fn get_or_create(&self, user_id: Option<&UserId>, channel: &str) -> Result<SessionId> {
        if user_id.is_none() {
            return self.create_session(None, channel);
        }

        let key = (
            user_id.map(|u| u.0.clone()).unwrap_or_default(),
            channel.to_string(),
        );

        // Check for existing active session
        {
            let user_sessions = self
                .user_sessions
                .read()
                .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;

            if let Some(session_id) = user_sessions.get(&key) {
                let sessions = self
                    .sessions
                    .read()
                    .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;

                if let Some(session) = sessions.get(session_id)
                    && (session.state == SessionState::Active
                        || session.state == SessionState::Idle)
                {
                    debug!(session_id = %session_id, "Reusing existing session");
                    return Ok(session_id.clone());
                }
            }
        }

        self.create_session(user_id.cloned(), channel)
    }

    fn create_session(&self, user_id: Option<UserId>, channel: &str) -> Result<SessionId> {
        let session_id = SessionId::new();
        let now = Utc::now();

        let session = Session {
            id: session_id.clone(),
            user_id: user_id.clone(),
            origin_channel: ChannelId(channel.to_string()),
            state: SessionState::Active,
            model: self.default_model.clone(),
            thinking_level: self.default_thinking_level,
            created_at: now,
            last_active: now,
            message_count: 0,
            tokens_used: 0,
        };

        {
            let mut sessions = self
                .sessions
                .write()
                .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;
            sessions.insert(session_id.clone(), session);
        }

        {
            if let Some(user_id) = user_id {
                let mut user_sessions = self
                    .user_sessions
                    .write()
                    .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;
                user_sessions.insert((user_id.0, channel.to_string()), session_id.clone());
            }
        }

        info!(session_id = %session_id, "Created new session");
        Ok(session_id)
    }

    /// Get a session by ID.
    pub fn get(&self, session_id: &SessionId) -> Option<Session> {
        let sessions = self.sessions.read().ok()?;
        sessions.get(session_id).cloned()
    }

    /// Update the session after a message was processed.
    pub fn record_message(&self, session_id: &SessionId, tokens_used: usize) -> Result<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;

        if let Some(session) = sessions.get_mut(session_id) {
            session.message_count += 1;
            session.tokens_used += tokens_used as u64;
            session.last_active = Utc::now();
            session.state = SessionState::Active;
        }

        Ok(())
    }

    /// Rebind an existing session to a canonical user.
    pub fn rebind_user(&self, session_id: &SessionId, user_id: &UserId) -> Result<()> {
        self.promote_to_user(session_id, user_id).map(|_| ())
    }

    /// Promote an existing session to a canonical user, reusing an already-active
    /// canonical session on the same channel when one exists.
    pub fn promote_to_user(&self, session_id: &SessionId, user_id: &UserId) -> Result<SessionId> {
        let current = self
            .get(session_id)
            .ok_or_else(|| Error::NotFound(format!("session {session_id}")))?;

        let channel = current.origin_channel.0.clone();
        let old_key = (
            current
                .user_id
                .as_ref()
                .map(|value| value.0.clone())
                .unwrap_or_default(),
            channel.clone(),
        );
        let new_key = (user_id.0.clone(), channel.clone());

        if old_key == new_key {
            self.touch_session(session_id)?;
            return Ok(session_id.clone());
        }

        if let Some(existing) = self.active_session_for_key(&new_key)?
            && existing != *session_id
        {
            self.end_session(session_id)?;
            self.detach_user_mapping_for_session(session_id, &old_key)?;
            self.touch_session(&existing)?;
            return Ok(existing);
        }

        let mut sessions = self
            .sessions
            .write()
            .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;

        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| Error::NotFound(format!("session {session_id}")))?;
        session.user_id = Some(user_id.clone());
        session.last_active = Utc::now();
        drop(sessions);

        let mut user_sessions = self
            .user_sessions
            .write()
            .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;
        if user_sessions.get(&old_key) == Some(session_id) {
            user_sessions.remove(&old_key);
        }
        user_sessions.insert(new_key, session_id.clone());
        Ok(session_id.clone())
    }

    /// Promote an alias-bound session to a canonical user for a given channel.
    pub fn promote_alias_to_user(
        &self,
        alias_user_id: &UserId,
        channel: &str,
        canonical_user_id: &UserId,
    ) -> Result<Option<SessionId>> {
        if alias_user_id == canonical_user_id {
            return Ok(None);
        }

        let key = (alias_user_id.0.clone(), channel.to_string());
        let session_id = {
            let user_sessions = self
                .user_sessions
                .read()
                .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;
            user_sessions.get(&key).cloned()
        };

        match session_id {
            Some(session_id) => self.promote_to_user(&session_id, canonical_user_id).map(Some),
            None => Ok(None),
        }
    }

    /// End a session.
    pub fn end_session(&self, session_id: &SessionId) -> Result<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;

        if let Some(session) = sessions.get_mut(session_id) {
            session.state = SessionState::Ended;
            info!(session_id = %session_id, "Session ended");
        }

        Ok(())
    }

    /// List all active sessions.
    pub fn active_sessions(&self) -> Vec<Session> {
        let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        sessions
            .values()
            .filter(|s| s.state == SessionState::Active || s.state == SessionState::Idle)
            .cloned()
            .collect()
    }

    /// Total number of sessions (including ended).
    pub fn total_sessions(&self) -> usize {
        self.sessions.read().map(|s| s.len()).unwrap_or(0)
    }

    /// Remove sessions that have been idle or ended for longer than `ttl`.
    ///
    /// Returns the number of pruned sessions.
    pub fn prune_expired(&self, ttl: std::time::Duration) -> usize {
        let cutoff =
            Utc::now() - chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(1));

        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        let mut user_sessions = self
            .user_sessions
            .write()
            .unwrap_or_else(|e| e.into_inner());

        let expired_ids: Vec<SessionId> = sessions
            .iter()
            .filter(|(_, s)| s.last_active < cutoff)
            .map(|(id, _)| id.clone())
            .collect();

        let count = expired_ids.len();
        for id in &expired_ids {
            if let Some(session) = sessions.remove(id) {
                // Clean up the user_sessions reverse map
                let key = (
                    session.user_id.map(|u| u.0).unwrap_or_default(),
                    session.origin_channel.0.clone(),
                );
                if user_sessions.get(&key) == Some(id) {
                    user_sessions.remove(&key);
                }
            }
        }

        if count > 0 {
            info!(
                pruned = count,
                remaining = sessions.len(),
                "Pruned expired sessions"
            );
        }

        count
    }

    fn touch_session(&self, session_id: &SessionId) -> Result<()> {
        let mut sessions = self
            .sessions
            .write()
            .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;

        if let Some(session) = sessions.get_mut(session_id) {
            session.last_active = Utc::now();
            if session.state == SessionState::Idle {
                session.state = SessionState::Active;
            }
        }

        Ok(())
    }

    fn active_session_for_key(&self, key: &(String, String)) -> Result<Option<SessionId>> {
        let session_id = {
            let user_sessions = self
                .user_sessions
                .read()
                .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;
            user_sessions.get(key).cloned()
        };

        match session_id {
            Some(session_id) => {
                let sessions = self
                    .sessions
                    .read()
                    .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;
                if let Some(session) = sessions.get(&session_id)
                    && (session.state == SessionState::Active
                        || session.state == SessionState::Idle)
                {
                    return Ok(Some(session_id));
                }
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn detach_user_mapping_for_session(
        &self,
        session_id: &SessionId,
        key: &(String, String),
    ) -> Result<()> {
        let mut user_sessions = self
            .user_sessions
            .write()
            .map_err(|e| Error::Gateway(format!("Session lock: {e}")))?;
        if user_sessions.get(key) == Some(session_id) {
            user_sessions.remove(key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager() -> SessionManager {
        SessionManager::new("ollama/llama3".to_string(), ThinkingLevel::Medium)
    }

    #[test]
    fn create_session() {
        let mgr = make_manager();
        let user = UserId("alice".into());
        let sid = mgr.get_or_create(Some(&user), "webchat").unwrap();
        let session = mgr.get(&sid).unwrap();
        assert_eq!(session.state, SessionState::Active);
        assert_eq!(session.user_id.unwrap().0, "alice");
    }

    #[test]
    fn reuse_active_session() {
        let mgr = make_manager();
        let user = UserId("alice".into());
        let sid1 = mgr.get_or_create(Some(&user), "webchat").unwrap();
        let sid2 = mgr.get_or_create(Some(&user), "webchat").unwrap();
        assert_eq!(sid1, sid2);
    }

    #[test]
    fn different_channel_gets_new_session() {
        let mgr = make_manager();
        let user = UserId("alice".into());
        let sid1 = mgr.get_or_create(Some(&user), "webchat").unwrap();
        let sid2 = mgr.get_or_create(Some(&user), "telegram").unwrap();
        assert_ne!(sid1, sid2);
    }

    #[test]
    fn record_message_updates_session() {
        let mgr = make_manager();
        let user = UserId("alice".into());
        let sid = mgr.get_or_create(Some(&user), "webchat").unwrap();
        mgr.record_message(&sid, 100).unwrap();
        let session = mgr.get(&sid).unwrap();
        assert_eq!(session.message_count, 1);
        assert_eq!(session.tokens_used, 100);
    }

    #[test]
    fn end_session_marks_ended() {
        let mgr = make_manager();
        let user = UserId("alice".into());
        let sid = mgr.get_or_create(Some(&user), "webchat").unwrap();
        mgr.end_session(&sid).unwrap();
        let session = mgr.get(&sid).unwrap();
        assert_eq!(session.state, SessionState::Ended);
    }

    #[test]
    fn ended_session_creates_new_one() {
        let mgr = make_manager();
        let user = UserId("alice".into());
        let sid1 = mgr.get_or_create(Some(&user), "webchat").unwrap();
        mgr.end_session(&sid1).unwrap();
        let sid2 = mgr.get_or_create(Some(&user), "webchat").unwrap();
        assert_ne!(sid1, sid2);
    }

    #[test]
    fn active_sessions_count() {
        let mgr = make_manager();
        let alice = UserId("alice".into());
        let bob = UserId("bob".into());
        mgr.get_or_create(Some(&alice), "webchat").unwrap();
        mgr.get_or_create(Some(&bob), "webchat").unwrap();
        assert_eq!(mgr.active_sessions().len(), 2);
    }

    #[test]
    fn anonymous_session() {
        let mgr = make_manager();
        let sid = mgr.get_or_create(None, "webchat").unwrap();
        let session = mgr.get(&sid).unwrap();
        assert!(session.user_id.is_none());
    }

    #[test]
    fn anonymous_sessions_do_not_reuse() {
        let mgr = make_manager();
        let sid1 = mgr.get_or_create(None, "webchat").unwrap();
        let sid2 = mgr.get_or_create(None, "webchat").unwrap();
        assert_ne!(sid1, sid2);
    }

    #[test]
    fn rebind_user_attaches_session_to_canonical_identity() {
        let mgr = make_manager();
        let sid = mgr.get_or_create(None, "webchat").unwrap();
        let user = UserId("alice".into());

        mgr.rebind_user(&sid, &user).unwrap();

        let rebound = mgr.get(&sid).unwrap();
        assert_eq!(rebound.user_id, Some(user.clone()));
        let reused = mgr.get_or_create(Some(&user), "webchat").unwrap();
        assert_eq!(reused, sid);
    }

    #[test]
    fn promote_to_user_reuses_existing_canonical_session() {
        let mgr = make_manager();
        let canonical = UserId("alice".into());
        let canonical_sid = mgr.get_or_create(Some(&canonical), "webchat").unwrap();
        let anonymous_sid = mgr.get_or_create(None, "webchat").unwrap();

        let promoted = mgr.promote_to_user(&anonymous_sid, &canonical).unwrap();

        assert_eq!(promoted, canonical_sid);
        assert_eq!(mgr.get(&anonymous_sid).unwrap().state, SessionState::Ended);
    }

    #[test]
    fn promote_alias_to_user_reuses_alias_session_when_available() {
        let mgr = make_manager();
        let alias = UserId("telegram:42".into());
        let canonical = UserId("alice".into());
        let alias_sid = mgr.get_or_create(Some(&alias), "telegram").unwrap();

        let promoted = mgr
            .promote_alias_to_user(&alias, "telegram", &canonical)
            .unwrap();

        assert_eq!(promoted, Some(alias_sid.clone()));
        assert_eq!(mgr.get(&alias_sid).unwrap().user_id, Some(canonical.clone()));
        assert_eq!(mgr.get_or_create(Some(&canonical), "telegram").unwrap(), alias_sid);
    }
}
