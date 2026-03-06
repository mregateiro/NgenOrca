//! Identity resolver — determines who is talking from any surface.

use ngenorca_core::types::{ChannelKind, DeviceId, TrustLevel, UserId};
use ngenorca_core::Result;

use crate::IdentityManager;

/// Resolution result from the identity system.
#[derive(Debug, Clone)]
pub struct ResolvedIdentity {
    /// User ID if resolved, None if unknown.
    pub user_id: Option<UserId>,
    /// Trust level of the identification.
    pub trust: TrustLevel,
    /// Whether the agent should proceed or challenge.
    pub action: IdentityAction,
}

/// What the gateway should do based on identity resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityAction {
    /// Identity confirmed — proceed with full access.
    Proceed,
    /// Identity confirmed but with reduced trust.
    ProceedReduced,
    /// Unknown identity — enter pairing flow.
    RequirePairing,
    /// Anomalous signal — challenge the sender.
    Challenge,
    /// Blocked sender.
    Block,
}

/// Resolve identity from a device-signed message.
pub fn resolve_from_device(
    manager: &IdentityManager,
    device_id: &DeviceId,
    _signature: &[u8],
    _message_bytes: &[u8],
) -> Result<ResolvedIdentity> {
    // TODO: Verify the signature against the stored public key.
    // For now, just look up the device binding.

    match manager.resolve_by_device(device_id)? {
        Some((user_id, trust)) => Ok(ResolvedIdentity {
            user_id: Some(user_id),
            trust,
            action: IdentityAction::Proceed,
        }),
        None => Ok(ResolvedIdentity {
            user_id: None,
            trust: TrustLevel::Unknown,
            action: IdentityAction::RequirePairing,
        }),
    }
}

/// Resolve identity from a channel handle (WhatsApp number, Telegram ID, etc).
pub fn resolve_from_channel(
    manager: &IdentityManager,
    channel_kind: &ChannelKind,
    handle: &str,
) -> Result<ResolvedIdentity> {
    match manager.resolve_by_channel(channel_kind, handle)? {
        Some((user_id, trust)) => Ok(ResolvedIdentity {
            user_id: Some(user_id),
            trust,
            action: IdentityAction::Proceed,
        }),
        None => Ok(ResolvedIdentity {
            user_id: None,
            trust: TrustLevel::Unknown,
            action: IdentityAction::RequirePairing,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::identity::UserRole;

    fn setup() -> IdentityManager {
        let mgr = IdentityManager::new(":memory:").unwrap();
        let uid = UserId("alice".into());
        mgr.register_user(uid.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();

        mgr.link_channel(
            &uid,
            ngenorca_core::types::ChannelId("tg-1".into()),
            ChannelKind::Telegram,
            "@alice".into(),
        )
        .unwrap();

        let device = ngenorca_core::identity::DeviceBinding {
            device_id: DeviceId("dev-1".into()),
            device_name: "Laptop".into(),
            attestation: ngenorca_core::identity::AttestationType::CompositeFingerprint,
            public_key_hash: "hash123".into(),
            trust: TrustLevel::Hardware,
            paired_at: chrono::Utc::now(),
            last_used: chrono::Utc::now(),
        };
        mgr.pair_device(&uid, device).unwrap();

        mgr
    }

    #[test]
    fn resolve_from_known_device() {
        let mgr = setup();
        let result = resolve_from_device(&mgr, &DeviceId("dev-1".into()), &[], &[]).unwrap();
        assert_eq!(result.user_id, Some(UserId("alice".into())));
        assert_eq!(result.action, IdentityAction::Proceed);
    }

    #[test]
    fn resolve_from_unknown_device() {
        let mgr = setup();
        let result = resolve_from_device(&mgr, &DeviceId("unknown".into()), &[], &[]).unwrap();
        assert!(result.user_id.is_none());
        assert_eq!(result.action, IdentityAction::RequirePairing);
        assert_eq!(result.trust, TrustLevel::Unknown);
    }

    #[test]
    fn resolve_from_known_channel() {
        let mgr = setup();
        let result = resolve_from_channel(&mgr, &ChannelKind::Telegram, "@alice").unwrap();
        assert_eq!(result.user_id, Some(UserId("alice".into())));
        assert_eq!(result.action, IdentityAction::Proceed);
        assert_eq!(result.trust, TrustLevel::Channel);
    }

    #[test]
    fn resolve_from_unknown_channel() {
        let mgr = setup();
        let result = resolve_from_channel(&mgr, &ChannelKind::Telegram, "@nobody").unwrap();
        assert!(result.user_id.is_none());
        assert_eq!(result.action, IdentityAction::RequirePairing);
    }

    #[test]
    fn identity_action_equality() {
        assert_eq!(IdentityAction::Proceed, IdentityAction::Proceed);
        assert_ne!(IdentityAction::Proceed, IdentityAction::Block);
        assert_ne!(IdentityAction::RequirePairing, IdentityAction::Challenge);
    }
}
