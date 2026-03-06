//! Core type aliases and newtypes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a user across all channels and devices.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

/// Unique identifier for a device (hardware-bound).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(pub String);

/// Unique identifier for a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

/// Unique identifier for a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginId(pub String);

/// Unique identifier for a channel adapter.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelId(pub String);

/// Unique identifier for an event in the event log.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub ulid::Ulid);

/// Trust level for identity verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TrustLevel {
    /// Unknown sender, no verification.
    Unknown = 0,
    /// Identified by channel handle only (WhatsApp number, Telegram ID, etc.).
    Channel = 1,
    /// Verified by client certificate (mTLS).
    Certificate = 2,
    /// Verified by hardware key (TPM/Secure Enclave).
    Hardware = 3,
}

/// The kind of channel an adapter provides.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelKind {
    WhatsApp,
    Telegram,
    Discord,
    Slack,
    Signal,
    IMessage,
    Matrix,
    IRC,
    WebChat,
    Teams,
    Custom(String),
}

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl EventId {
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_equality() {
        let a = UserId("alice".into());
        let b = UserId("alice".into());
        let c = UserId("bob".into());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn device_id_equality() {
        let a = DeviceId("dev-1".into());
        let b = DeviceId("dev-1".into());
        assert_eq!(a, b);
    }

    #[test]
    fn session_id_unique() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b, "Two new SessionIds should be distinct");
    }

    #[test]
    fn session_id_default_is_new() {
        let a = SessionId::default();
        let b = SessionId::default();
        assert_ne!(a, b);
    }

    #[test]
    fn event_id_unique() {
        let a = EventId::new();
        let b = EventId::new();
        assert_ne!(a, b, "Two new EventIds should be distinct");
    }

    #[test]
    fn event_id_default_is_new() {
        let a = EventId::default();
        let b = EventId::default();
        assert_ne!(a, b);
    }

    #[test]
    fn trust_level_ordering() {
        assert!(TrustLevel::Unknown < TrustLevel::Channel);
        assert!(TrustLevel::Channel < TrustLevel::Certificate);
        assert!(TrustLevel::Certificate < TrustLevel::Hardware);
    }

    #[test]
    fn trust_level_serde_roundtrip() {
        let levels = vec![
            TrustLevel::Unknown,
            TrustLevel::Channel,
            TrustLevel::Certificate,
            TrustLevel::Hardware,
        ];
        for level in &levels {
            let json = serde_json::to_string(level).unwrap();
            let back: TrustLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(*level, back);
        }
    }

    #[test]
    fn channel_kind_serde_roundtrip() {
        let kinds = vec![
            ChannelKind::WhatsApp,
            ChannelKind::Telegram,
            ChannelKind::Discord,
            ChannelKind::WebChat,
            ChannelKind::Custom("MyApp".into()),
        ];
        for kind in &kinds {
            let json = serde_json::to_string(kind).unwrap();
            let back: ChannelKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }

    #[test]
    fn user_id_display() {
        let uid = UserId("miguel".into());
        assert_eq!(uid.to_string(), "miguel");
    }

    #[test]
    fn session_id_display() {
        let sid = SessionId(uuid::Uuid::nil());
        assert_eq!(sid.to_string(), "00000000-0000-0000-0000-000000000000");
    }

    #[test]
    fn event_id_display() {
        let eid = EventId::new();
        let s = eid.to_string();
        assert!(!s.is_empty());
        // ULID is 26 chars
        assert_eq!(s.len(), 26);
    }

    #[test]
    fn user_id_serde_roundtrip() {
        let uid = UserId("hello".into());
        let json = serde_json::to_string(&uid).unwrap();
        let back: UserId = serde_json::from_str(&json).unwrap();
        assert_eq!(uid, back);
    }

    #[test]
    fn session_id_serde_roundtrip() {
        let sid = SessionId::new();
        let json = serde_json::to_string(&sid).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(sid, back);
    }

    #[test]
    fn event_id_serde_roundtrip() {
        let eid = EventId::new();
        let json = serde_json::to_string(&eid).unwrap();
        let back: EventId = serde_json::from_str(&json).unwrap();
        assert_eq!(eid, back);
    }

    #[test]
    fn channel_kind_custom_preserves_value() {
        let kind = ChannelKind::Custom("Rocket.Chat".into());
        if let ChannelKind::Custom(ref s) = kind {
            assert_eq!(s, "Rocket.Chat");
        } else {
            panic!("Expected Custom variant");
        }
    }
}
