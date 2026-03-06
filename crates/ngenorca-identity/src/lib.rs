//! # NgenOrca Identity
//!
//! Hardware-bound identity system that:
//!
//! 1. Generates keypairs inside TPM/Secure Enclave/StrongBox on pairing
//! 2. Signs every message from paired devices for cryptographic verification
//! 3. Unifies channel handles (WhatsApp, Telegram, etc.) into a single user
//! 4. Supports multi-user households with isolated memory profiles
//! 5. Adapts agent behavior based on trust level

pub mod fingerprint;
pub mod resolver;
pub mod store;

use ngenorca_core::identity::{UserIdentity, UserRole};
use ngenorca_core::types::{ChannelId, ChannelKind, DeviceId, TrustLevel, UserId};
use ngenorca_core::Result;
use store::IdentityStore;
use tracing::info;

/// The identity manager — resolves who is talking from any surface.
pub struct IdentityManager {
    pub(crate) store: IdentityStore,
}

impl IdentityManager {
    /// Create a new identity manager backed by the given SQLite database.
    pub fn new(db_path: &str) -> Result<Self> {
        let store = IdentityStore::new(db_path)?;
        info!("Identity manager initialized");
        Ok(Self { store })
    }

    /// Resolve a user from a device signature.
    ///
    /// This is the highest-trust path: the message was signed by a hardware key.
    pub fn resolve_by_device(&self, device_id: &DeviceId) -> Result<Option<(UserId, TrustLevel)>> {
        if let Some(identity) = self.store.find_by_device(device_id)? {
            let trust = identity
                .devices
                .iter()
                .find(|d| &d.device_id == device_id)
                .map(|d| d.trust)
                .unwrap_or(TrustLevel::Channel);

            Ok(Some((identity.user_id, trust)))
        } else {
            Ok(None)
        }
    }

    /// Resolve a user from a channel handle (e.g., WhatsApp phone number).
    ///
    /// Medium trust — we trust the channel platform's identity.
    pub fn resolve_by_channel(
        &self,
        channel_kind: &ChannelKind,
        handle: &str,
    ) -> Result<Option<(UserId, TrustLevel)>> {
        if let Some(identity) = self.store.find_by_channel(channel_kind, handle)? {
            Ok(Some((identity.user_id, TrustLevel::Channel)))
        } else {
            Ok(None)
        }
    }

    /// Register a new user (typically the owner on first setup).
    pub fn register_user(
        &self,
        user_id: UserId,
        display_name: String,
        role: UserRole,
    ) -> Result<UserIdentity> {
        let identity = UserIdentity {
            user_id: user_id.clone(),
            display_name,
            role,
            devices: vec![],
            channels: vec![],
            created_at: chrono::Utc::now(),
            last_seen: chrono::Utc::now(),
        };

        self.store.save_user(&identity)?;
        info!(user = %user_id, "User registered");
        Ok(identity)
    }

    /// Pair a device to a user (hardware-bound identity).
    pub fn pair_device(
        &self,
        user_id: &UserId,
        device: ngenorca_core::identity::DeviceBinding,
    ) -> Result<()> {
        self.store.add_device(user_id, &device)?;
        info!(
            user = %user_id,
            device = %device.device_id.0,
            attestation = ?device.attestation,
            "Device paired"
        );
        Ok(())
    }

    /// Link a channel handle to a user.
    pub fn link_channel(
        &self,
        user_id: &UserId,
        channel_id: ChannelId,
        channel_kind: ChannelKind,
        handle: String,
    ) -> Result<()> {
        let binding = ngenorca_core::identity::ChannelBinding {
            channel_id,
            channel_kind: channel_kind.clone(),
            handle: handle.clone(),
            trust: TrustLevel::Channel,
            linked_at: chrono::Utc::now(),
        };

        self.store.add_channel(user_id, &binding)?;
        info!(
            user = %user_id,
            channel = ?channel_kind,
            handle = %handle,
            "Channel linked"
        );
        Ok(())
    }

    /// Get full identity for a user.
    pub fn get_user(&self, user_id: &UserId) -> Result<Option<UserIdentity>> {
        self.store.get_user(user_id)
    }

    /// List all known users.
    pub fn list_users(&self) -> Result<Vec<UserIdentity>> {
        self.store.list_users()
    }

    /// Revoke a device binding.
    pub fn revoke_device(&self, user_id: &UserId, device_id: &DeviceId) -> Result<()> {
        self.store.remove_device(user_id, device_id)?;
        info!(user = %user_id, device = %device_id.0, "Device revoked");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::identity::UserRole;

    fn in_memory_manager() -> IdentityManager {
        IdentityManager::new(":memory:").unwrap()
    }

    #[test]
    fn register_and_get_user() {
        let mgr = in_memory_manager();
        let uid = UserId("alice".into());
        let identity = mgr
            .register_user(uid.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();
        assert_eq!(identity.user_id, uid);
        assert_eq!(identity.display_name, "Alice");
        assert_eq!(identity.role, UserRole::Owner);

        let fetched = mgr.get_user(&uid).unwrap().unwrap();
        assert_eq!(fetched.user_id, uid);
        assert_eq!(fetched.display_name, "Alice");
    }

    #[test]
    fn list_users_returns_all() {
        let mgr = in_memory_manager();
        mgr.register_user(UserId("alice".into()), "Alice".into(), UserRole::Owner)
            .unwrap();
        mgr.register_user(UserId("bob".into()), "Bob".into(), UserRole::Family)
            .unwrap();

        let users = mgr.list_users().unwrap();
        assert_eq!(users.len(), 2);
    }

    #[test]
    fn get_nonexistent_user_returns_none() {
        let mgr = in_memory_manager();
        let result = mgr.get_user(&UserId("nobody".into())).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn link_channel_and_resolve() {
        let mgr = in_memory_manager();
        let uid = UserId("alice".into());
        mgr.register_user(uid.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();

        mgr.link_channel(
            &uid,
            ChannelId("tg-1".into()),
            ChannelKind::Telegram,
            "@alice".into(),
        )
        .unwrap();

        let (resolved_uid, trust) = mgr
            .resolve_by_channel(&ChannelKind::Telegram, "@alice")
            .unwrap()
            .unwrap();
        assert_eq!(resolved_uid, uid);
        assert_eq!(trust, TrustLevel::Channel);
    }

    #[test]
    fn resolve_unknown_channel_returns_none() {
        let mgr = in_memory_manager();
        let result = mgr
            .resolve_by_channel(&ChannelKind::Telegram, "@unknown")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn pair_device_and_resolve() {
        let mgr = in_memory_manager();
        let uid = UserId("alice".into());
        mgr.register_user(uid.clone(), "Alice".into(), UserRole::Owner)
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

        let (resolved_uid, trust) = mgr
            .resolve_by_device(&DeviceId("dev-1".into()))
            .unwrap()
            .unwrap();
        assert_eq!(resolved_uid, uid);
        assert_eq!(trust, TrustLevel::Hardware);
    }

    #[test]
    fn resolve_unknown_device_returns_none() {
        let mgr = in_memory_manager();
        let result = mgr
            .resolve_by_device(&DeviceId("nonexistent".into()))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn revoke_device_works() {
        let mgr = in_memory_manager();
        let uid = UserId("alice".into());
        mgr.register_user(uid.clone(), "Alice".into(), UserRole::Owner)
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

        // Verify device exists
        assert!(mgr.resolve_by_device(&DeviceId("dev-1".into())).unwrap().is_some());

        // Revoke it
        mgr.revoke_device(&uid, &DeviceId("dev-1".into())).unwrap();

        // Should no longer resolve
        assert!(mgr.resolve_by_device(&DeviceId("dev-1".into())).unwrap().is_none());
    }
}
