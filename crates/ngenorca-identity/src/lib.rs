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

use ngenorca_core::Result;
use ngenorca_core::identity::{UserIdentity, UserRole};
use ngenorca_core::{Error, identity::{AttestationType, DeviceBinding}};
use ngenorca_core::types::{ChannelId, ChannelKind, DeviceId, TrustLevel, UserId};
use resolver::channel_handle_candidates;
use serde::{Deserialize, Serialize};
use store::IdentityStore;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingRequest {
    pub request_id: String,
    pub channel_kind: Option<ChannelKind>,
    pub handle: Option<String>,
    pub device_id: Option<DeviceId>,
    pub device_name: Option<String>,
    pub requested_user_id: Option<UserId>,
    pub requested_display_name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
pub struct PairingSeed {
    pub channel_kind: Option<ChannelKind>,
    pub handle: Option<String>,
    pub device_id: Option<DeviceId>,
    pub device_name: Option<String>,
    pub requested_user_id: Option<UserId>,
    pub requested_display_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PairingCompletion {
    pub user_id: UserId,
    pub display_name: Option<String>,
    pub role: UserRole,
    pub channel_id: Option<ChannelId>,
    pub device_name: Option<String>,
    pub attestation: Option<AttestationType>,
    pub public_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeRequest {
    pub request_id: String,
    pub user_id: Option<UserId>,
    pub device_id: DeviceId,
    pub nonce_b64: String,
    pub reason: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub verified_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
pub struct ChallengeSeed {
    pub device_id: DeviceId,
    pub user_id: Option<UserId>,
    pub reason: String,
}

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
        for candidate in channel_handle_candidates(channel_kind, handle) {
            if let Some(identity) = self.store.find_by_channel(channel_kind, &candidate)? {
                return Ok(Some((identity.user_id, TrustLevel::Channel)));
            }
        }

        Ok(None)
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
        let candidates = channel_handle_candidates(&channel_kind, &handle);

        for candidate in &candidates {
            if let Some(existing) = self.store.find_by_channel(&channel_kind, &candidate)?
                && existing.user_id != *user_id
            {
                return Err(Error::Identity(format!(
                    "channel handle '{}' conflicts with existing {:?} binding for user {}",
                    handle,
                    channel_kind,
                    existing.user_id.0
                )));
            }
        }

        for candidate in candidates {
            let binding = ngenorca_core::identity::ChannelBinding {
                channel_id: channel_id.clone(),
                channel_kind: channel_kind.clone(),
                handle: candidate,
                trust: TrustLevel::Channel,
                linked_at: chrono::Utc::now(),
            };

            self.store.add_channel(user_id, &binding)?;
        }
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

    pub fn issue_pairing_request(&self, seed: PairingSeed) -> Result<PairingRequest> {
        let now = chrono::Utc::now();
        let request = PairingRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            channel_kind: seed.channel_kind,
            handle: seed.handle,
            device_id: seed.device_id,
            device_name: seed.device_name,
            requested_user_id: seed.requested_user_id,
            requested_display_name: seed.requested_display_name,
            created_at: now,
            expires_at: now + chrono::Duration::hours(1),
            completed_at: None,
        };

        self.store.save_pairing_request(&request)?;
        Ok(request)
    }

    pub fn get_pairing_request(&self, request_id: &str) -> Result<Option<PairingRequest>> {
        self.store.get_pairing_request(request_id)
    }

    pub fn complete_pairing_request(
        &self,
        request_id: &str,
        completion: PairingCompletion,
    ) -> Result<UserIdentity> {
        let request = self
            .store
            .get_pairing_request(request_id)?
            .ok_or_else(|| Error::NotFound(format!("pairing request {request_id}")))?;

        if request.completed_at.is_some() {
            return Err(Error::Identity(format!(
                "pairing request {request_id} is already completed"
            )));
        }
        if request.expires_at < chrono::Utc::now() {
            return Err(Error::Identity(format!(
                "pairing request {request_id} has expired"
            )));
        }

        let user_id = completion.user_id.clone();
        if self.get_user(&user_id)?.is_none() {
            let display_name = completion
                .display_name
                .clone()
                .or_else(|| request.requested_display_name.clone())
                .unwrap_or_else(|| user_id.0.clone());
            self.register_user(user_id.clone(), display_name, completion.role)?;
        }

        if let (Some(channel_kind), Some(handle)) = (&request.channel_kind, &request.handle) {
            self.link_channel(
                &user_id,
                completion
                    .channel_id
                    .clone()
                    .unwrap_or_else(|| ChannelId(format!("{channel_kind}:{handle}"))),
                channel_kind.clone(),
                handle.clone(),
            )?;
        }

        if let Some(device_id) = request.device_id.clone() {
            let public_key = completion.public_key.clone().ok_or_else(|| {
                Error::Identity(format!(
                    "pairing request {request_id} requires a public key to bind device {}",
                    device_id.0
                ))
            })?;
            let device = DeviceBinding {
                device_id,
                device_name: completion
                    .device_name
                    .clone()
                    .or_else(|| request.device_name.clone())
                    .unwrap_or_else(|| "Paired device".into()),
                attestation: completion
                    .attestation
                    .clone()
                    .unwrap_or(AttestationType::CompositeFingerprint),
                public_key_hash: public_key,
                trust: TrustLevel::Hardware,
                paired_at: chrono::Utc::now(),
                last_used: chrono::Utc::now(),
            };
            self.pair_device(&user_id, device)?;
        }

        self.store.mark_pairing_request_completed(request_id)?;
        self.get_user(&user_id)?
            .ok_or_else(|| Error::NotFound(format!("paired user {}", user_id.0)))
    }

    pub fn issue_challenge_request(&self, seed: ChallengeSeed) -> Result<ChallengeRequest> {
        let mut nonce = [0u8; 32];
        let rng = ring::rand::SystemRandom::new();
        use ring::rand::SecureRandom;
        rng.fill(&mut nonce)
            .map_err(|_| Error::Identity("failed to generate challenge nonce".into()))?;
        let now = chrono::Utc::now();
        let request = ChallengeRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            user_id: seed.user_id,
            device_id: seed.device_id,
            nonce_b64: {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(nonce)
            },
            reason: seed.reason,
            created_at: now,
            expires_at: now + chrono::Duration::minutes(15),
            verified_at: None,
        };

        self.store.save_challenge_request(&request)?;
        Ok(request)
    }

    pub fn get_challenge_request(&self, request_id: &str) -> Result<Option<ChallengeRequest>> {
        self.store.get_challenge_request(request_id)
    }

    pub fn verify_challenge_response(
        &self,
        request_id: &str,
        signature_b64: &str,
    ) -> Result<(UserId, TrustLevel)> {
        let request = self
            .store
            .get_challenge_request(request_id)?
            .ok_or_else(|| Error::NotFound(format!("challenge request {request_id}")))?;

        if request.verified_at.is_some() {
            return Err(Error::Identity(format!(
                "challenge request {request_id} is already verified"
            )));
        }
        if request.expires_at < chrono::Utc::now() {
            return Err(Error::Identity(format!(
                "challenge request {request_id} has expired"
            )));
        }

        let signature = decode_base64(signature_b64)?;
        let message = decode_base64(&request.nonce_b64)?;
        if !self.verify_signature_for_device(&request.device_id, &signature, &message)? {
            return Err(Error::Unauthorized(format!(
                "challenge response did not verify for device {}",
                request.device_id.0
            )));
        }

        self.store.mark_challenge_request_verified(request_id)?;
        self.resolve_by_device(&request.device_id)?
            .ok_or_else(|| Error::NotFound(format!("device {}", request.device_id.0)))
    }

    pub fn verify_signature_for_device(
        &self,
        device_id: &DeviceId,
        signature: &[u8],
        message: &[u8],
    ) -> Result<bool> {
        let identity = self
            .store
            .find_by_device(device_id)?
            .ok_or_else(|| Error::NotFound(format!("device {}", device_id.0)))?;
        let device = identity
            .devices
            .iter()
            .find(|device| device.device_id == *device_id)
            .ok_or_else(|| Error::NotFound(format!("device binding {}", device_id.0)))?;
        verify_device_signature(&device.public_key_hash, signature, message)
    }
}

fn decode_base64(value: &str) -> Result<Vec<u8>> {
    use base64::Engine;

    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(value.trim()))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(value.trim()))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value.trim()))
        .map_err(|e| Error::Identity(format!("invalid base64 payload: {e}")))
}

fn verify_device_signature(public_key_b64: &str, signature_bytes: &[u8], message: &[u8]) -> Result<bool> {
    use base64::Engine;
    use ring::signature::{ED25519, UnparsedPublicKey};

    let pub_bytes = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64)
        .map_err(|e| Error::Identity(format!("invalid public key encoding: {e}")))?;
    let public_key = UnparsedPublicKey::new(&ED25519, &pub_bytes);
    Ok(public_key.verify(message, signature_bytes).is_ok())
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
    fn resolve_by_channel_accepts_normalized_alias_forms() {
        let mgr = in_memory_manager();
        let uid = UserId("alice".into());
        mgr.register_user(uid.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();

        mgr.link_channel(
            &uid,
            ChannelId("wa-1".into()),
            ChannelKind::WhatsApp,
            "+15550100200".into(),
        )
        .unwrap();

        let resolved = mgr
            .resolve_by_channel(&ChannelKind::WhatsApp, "whatsapp:1 (555) 010-0200")
            .unwrap()
            .unwrap();
        assert_eq!(resolved.0, uid);
    }

    #[test]
    fn link_channel_rejects_equivalent_alias_conflicts() {
        let mgr = in_memory_manager();
        let alice = UserId("alice".into());
        let bob = UserId("bob".into());
        mgr.register_user(alice.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();
        mgr.register_user(bob.clone(), "Bob".into(), UserRole::Family)
            .unwrap();

        mgr.link_channel(
            &alice,
            ChannelId("tg-alice".into()),
            ChannelKind::Telegram,
            "@Alice".into(),
        )
        .unwrap();

        let error = mgr
            .link_channel(
                &bob,
                ChannelId("tg-bob".into()),
                ChannelKind::Telegram,
                "alice".into(),
            )
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("conflicts with existing"));
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
        assert!(
            mgr.resolve_by_device(&DeviceId("dev-1".into()))
                .unwrap()
                .is_some()
        );

        // Revoke it
        mgr.revoke_device(&uid, &DeviceId("dev-1".into())).unwrap();

        // Should no longer resolve
        assert!(
            mgr.resolve_by_device(&DeviceId("dev-1".into()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn pairing_request_can_create_user_and_bind_channel() {
        let mgr = in_memory_manager();
        let request = mgr
            .issue_pairing_request(PairingSeed {
                channel_kind: Some(ChannelKind::WebChat),
                handle: Some("alice".into()),
                device_id: None,
                device_name: None,
                requested_user_id: Some(UserId("alice".into())),
                requested_display_name: Some("Alice".into()),
            })
            .unwrap();

        let user = mgr
            .complete_pairing_request(
                &request.request_id,
                PairingCompletion {
                    user_id: UserId("alice".into()),
                    display_name: Some("Alice".into()),
                    role: UserRole::Owner,
                    channel_id: Some(ChannelId("webchat:alice".into())),
                    device_name: None,
                    attestation: None,
                    public_key: None,
                },
            )
            .unwrap();

        assert_eq!(user.user_id.0, "alice");
        assert_eq!(user.channels.len(), 1);
    }

    #[test]
    fn challenge_request_verifies_signed_nonce() {
        use base64::Engine;
        use ring::signature::{Ed25519KeyPair, KeyPair as _};

        let mgr = in_memory_manager();
        let user_id = UserId("alice".into());
        mgr.register_user(user_id.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        mgr.pair_device(
            &user_id,
            DeviceBinding {
                device_id: DeviceId("dev-1".into()),
                device_name: "Laptop".into(),
                attestation: AttestationType::CompositeFingerprint,
                public_key_hash: base64::engine::general_purpose::STANDARD
                    .encode(key_pair.public_key().as_ref()),
                trust: TrustLevel::Hardware,
                paired_at: chrono::Utc::now(),
                last_used: chrono::Utc::now(),
            },
        )
        .unwrap();

        let challenge = mgr
            .issue_challenge_request(ChallengeSeed {
                device_id: DeviceId("dev-1".into()),
                user_id: Some(user_id.clone()),
                reason: "test".into(),
            })
            .unwrap();
        let nonce = base64::engine::general_purpose::STANDARD
            .decode(&challenge.nonce_b64)
            .unwrap();
        let signature = base64::engine::general_purpose::STANDARD
            .encode(key_pair.sign(&nonce).as_ref());

        let (resolved_user, trust) = mgr
            .verify_challenge_response(&challenge.request_id, &signature)
            .unwrap();
        assert_eq!(resolved_user, user_id);
        assert_eq!(trust, TrustLevel::Hardware);
    }
}
