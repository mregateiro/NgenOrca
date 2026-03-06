//! Identity types and traits for hardware-bound user identification.

use serde::{Deserialize, Serialize};

use crate::types::{ChannelId, ChannelKind, DeviceId, TrustLevel, UserId};

/// A unified user identity that spans all channels and devices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIdentity {
    /// The unique user ID (chosen by the user or auto-generated).
    pub user_id: UserId,

    /// Display name.
    pub display_name: String,

    /// Role within this NgenOrca instance.
    pub role: UserRole,

    /// All devices bound to this user.
    pub devices: Vec<DeviceBinding>,

    /// All channel handles linked to this user.
    pub channels: Vec<ChannelBinding>,

    /// When this identity was created.
    pub created_at: chrono::DateTime<chrono::Utc>,

    /// When this identity was last seen.
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

/// Role of a user within the NgenOrca instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserRole {
    /// Full control — can manage plugins, users, config.
    Owner,
    /// Can interact with the assistant, limited admin.
    Family,
    /// Can interact but with restricted capabilities.
    Guest,
}

/// A hardware device bound to a user identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceBinding {
    /// Unique device ID.
    pub device_id: DeviceId,

    /// Human-readable device name (e.g., "Alex's MacBook Pro").
    pub device_name: String,

    /// The kind of hardware attestation used.
    pub attestation: AttestationType,

    /// Public key hash (from TPM/Secure Enclave/StrongBox).
    pub public_key_hash: String,

    /// Trust level this device provides.
    pub trust: TrustLevel,

    /// When this device was paired.
    pub paired_at: chrono::DateTime<chrono::Utc>,

    /// Last time this device was used.
    pub last_used: chrono::DateTime<chrono::Utc>,
}

/// How the device proves its identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttestationType {
    /// Windows TPM 2.0.
    Tpm,
    /// macOS/iOS Secure Enclave.
    SecureEnclave,
    /// Android StrongBox / hardware keystore.
    StrongBox,
    /// Client certificate (mTLS).
    ClientCertificate,
    /// Composite fingerprint (fallback).
    CompositeFingerprint,
}

/// A channel handle linked to a user identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelBinding {
    /// The channel this binding is for.
    pub channel_id: ChannelId,

    /// Kind of channel.
    pub channel_kind: ChannelKind,

    /// The handle on that channel (phone number, username, etc.).
    pub handle: String,

    /// Trust level of this channel binding.
    pub trust: TrustLevel,

    /// When this channel was linked.
    pub linked_at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    #[test]
    fn user_role_serde_roundtrip() {
        let roles = vec![UserRole::Owner, UserRole::Family, UserRole::Guest];
        for role in &roles {
            let json = serde_json::to_string(role).unwrap();
            let back: UserRole = serde_json::from_str(&json).unwrap();
            assert_eq!(*role, back);
        }
    }

    #[test]
    fn attestation_type_serde_roundtrip() {
        let types = vec![
            AttestationType::Tpm,
            AttestationType::SecureEnclave,
            AttestationType::StrongBox,
            AttestationType::ClientCertificate,
            AttestationType::CompositeFingerprint,
        ];
        for at in &types {
            let json = serde_json::to_string(at).unwrap();
            let back: AttestationType = serde_json::from_str(&json).unwrap();
            assert_eq!(*at, back);
        }
    }

    #[test]
    fn user_identity_serde_roundtrip() {
        let identity = UserIdentity {
            user_id: UserId("alice".into()),
            display_name: "Alice".into(),
            role: UserRole::Owner,
            devices: vec![DeviceBinding {
                device_id: DeviceId("dev-1".into()),
                device_name: "Laptop".into(),
                attestation: AttestationType::CompositeFingerprint,
                public_key_hash: "abc123".into(),
                trust: TrustLevel::Certificate,
                paired_at: chrono::Utc::now(),
                last_used: chrono::Utc::now(),
            }],
            channels: vec![ChannelBinding {
                channel_id: ChannelId("tg-1".into()),
                channel_kind: ChannelKind::Telegram,
                handle: "@alice".into(),
                trust: TrustLevel::Channel,
                linked_at: chrono::Utc::now(),
            }],
            created_at: chrono::Utc::now(),
            last_seen: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&identity).unwrap();
        let back: UserIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(identity.user_id, back.user_id);
        assert_eq!(identity.display_name, back.display_name);
        assert_eq!(identity.role, back.role);
        assert_eq!(identity.devices.len(), 1);
        assert_eq!(identity.channels.len(), 1);
    }

    #[test]
    fn device_binding_serde_roundtrip() {
        let db = DeviceBinding {
            device_id: DeviceId("d-1".into()),
            device_name: "Phone".into(),
            attestation: AttestationType::Tpm,
            public_key_hash: "hash".into(),
            trust: TrustLevel::Hardware,
            paired_at: chrono::Utc::now(),
            last_used: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&db).unwrap();
        let back: DeviceBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(db.device_id, back.device_id);
        assert_eq!(db.attestation, back.attestation);
        assert_eq!(db.trust, back.trust);
    }

    #[test]
    fn channel_binding_serde_roundtrip() {
        let cb = ChannelBinding {
            channel_id: ChannelId("wa-1".into()),
            channel_kind: ChannelKind::WhatsApp,
            handle: "+351912345678".into(),
            trust: TrustLevel::Channel,
            linked_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&cb).unwrap();
        let back: ChannelBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(cb.channel_id, back.channel_id);
        assert_eq!(cb.handle, back.handle);
    }
}
