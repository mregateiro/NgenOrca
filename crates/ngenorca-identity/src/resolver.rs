//! Identity resolver — determines who is talking from any surface.

use ngenorca_core::Result;
use ngenorca_core::types::{ChannelKind, DeviceId, TrustLevel, UserId};

use crate::IdentityManager;

pub fn channel_handle_candidates(channel_kind: &ChannelKind, handle: &str) -> Vec<String> {
    let mut values = Vec::new();
    let trimmed = handle.trim();
    if trimmed.is_empty() {
        return values;
    }

    push_basic_candidates(&mut values, trimmed);

    let unprefixed = strip_channel_prefix(trimmed);
    push_basic_candidates(&mut values, unprefixed);

    match channel_kind {
        ChannelKind::Telegram | ChannelKind::Discord | ChannelKind::IRC => {
            push_username_candidates(&mut values, unprefixed);
            push_bracketed_mention_candidates(&mut values, unprefixed);
        }
        ChannelKind::WhatsApp | ChannelKind::Signal | ChannelKind::IMessage => {
            push_phone_candidates(&mut values, unprefixed);
        }
        ChannelKind::Matrix => {
            push_matrix_candidates(&mut values, unprefixed);
        }
        ChannelKind::Slack => {
            push_basic_candidates(&mut values, strip_slack_mention(unprefixed));
            push_bracketed_mention_candidates(&mut values, unprefixed);
            push_username_candidates(&mut values, unprefixed);
        }
        ChannelKind::Teams => {
            let stripped = strip_teams_prefix(unprefixed);
            push_basic_candidates(&mut values, stripped);
            push_username_candidates(&mut values, stripped);
            push_email_candidates(&mut values, stripped);
        }
        ChannelKind::WebChat => {
            push_web_candidates(&mut values, unprefixed);
        }
        ChannelKind::Custom(_) => {
            push_username_candidates(&mut values, unprefixed);
            push_phone_candidates(&mut values, unprefixed);
            push_email_candidates(&mut values, unprefixed);
        }
    }

    values
}

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
///
/// If the device is known **and** `signature` + `message_bytes` are non-empty,
/// the signature is verified against the stored public key (base64-encoded
/// Ed25519 in `DeviceBinding.public_key_hash`). Verification failure downgrades
/// the result to `Challenge`.
///
/// When `signature` is empty the device is still resolved (backward compat),
/// but trust is capped at `TrustLevel::Channel`.
pub fn resolve_from_device(
    manager: &IdentityManager,
    device_id: &DeviceId,
    signature: &[u8],
    message_bytes: &[u8],
) -> Result<ResolvedIdentity> {
    match manager.resolve_by_device(device_id)? {
        Some((user_id, stored_trust)) => {
            // If no signature provided, accept but at reduced trust.
            if signature.is_empty() || message_bytes.is_empty() {
                let trust = stored_trust.min(TrustLevel::Channel);
                return Ok(ResolvedIdentity {
                    user_id: Some(user_id),
                    trust,
                    action: IdentityAction::ProceedReduced,
                });
            }

            // Attempt to verify the signature.
            let identity = manager
                .store
                .find_by_device(device_id)?
                .expect("device was just resolved");

            let device = identity
                .devices
                .iter()
                .find(|d| d.device_id == *device_id)
                .expect("device binding exists");

            match verify_device_signature(&device.public_key_hash, signature, message_bytes) {
                Ok(true) => Ok(ResolvedIdentity {
                    user_id: Some(user_id),
                    trust: stored_trust,
                    action: IdentityAction::Proceed,
                }),
                Ok(false) => {
                    tracing::warn!(
                        device_id = %device_id.0,
                        "Signature verification failed"
                    );
                    Ok(ResolvedIdentity {
                        user_id: Some(user_id),
                        trust: TrustLevel::Unknown,
                        action: IdentityAction::Challenge,
                    })
                }
                Err(_) => {
                    // Key decode error — treat as verification failure.
                    tracing::warn!(
                        device_id = %device_id.0,
                        "Public key decode failed, challenging device"
                    );
                    Ok(ResolvedIdentity {
                        user_id: Some(user_id),
                        trust: TrustLevel::Unknown,
                        action: IdentityAction::Challenge,
                    })
                }
            }
        }
        None => Ok(ResolvedIdentity {
            user_id: None,
            trust: TrustLevel::Unknown,
            action: IdentityAction::RequirePairing,
        }),
    }
}

/// Verify an Ed25519 signature against a base64-encoded public key.
///
/// Returns `Ok(true)` if the signature is valid, `Ok(false)` if the
/// signature is invalid, and `Err` if the public key cannot be decoded.
fn verify_device_signature(
    public_key_b64: &str,
    signature: &[u8],
    message: &[u8],
) -> std::result::Result<bool, base64::DecodeError> {
    use base64::Engine;
    use ring::signature;

    let pub_bytes = base64::engine::general_purpose::STANDARD.decode(public_key_b64)?;
    let public_key = signature::UnparsedPublicKey::new(&signature::ED25519, &pub_bytes);

    Ok(public_key.verify(message, signature).is_ok())
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

fn push_basic_candidates(values: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }

    push_unique(values, trimmed.to_string());

    let lowercase = trimmed.to_ascii_lowercase();
    if lowercase != trimmed {
        push_unique(values, lowercase);
    }
}

fn push_username_candidates(values: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim().trim_start_matches('@');
    if trimmed.is_empty() {
        return;
    }

    push_basic_candidates(values, trimmed);
    push_basic_candidates(values, &format!("@{trimmed}"));
}

fn push_phone_candidates(values: &mut Vec<String>, raw: &str) {
    let trimmed = raw
        .trim()
        .trim_start_matches("tel:")
        .trim_start_matches("sms:")
        .trim_start_matches("waid:");
    let digits: String = trimmed.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        return;
    }

    push_unique(values, digits.clone());
    push_unique(values, format!("+{digits}"));
}

fn push_matrix_candidates(values: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return;
    }

    push_basic_candidates(values, trimmed);

    if let Some((local_part, _)) = trimmed.split_once(':') {
        push_username_candidates(values, local_part);
    } else {
        push_username_candidates(values, trimmed);
    }
}

fn push_web_candidates(values: &mut Vec<String>, raw: &str) {
    push_basic_candidates(values, raw);
    push_email_candidates(values, raw);
}

fn push_email_candidates(values: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim();
    if let Some((local_part, _)) = trimmed.split_once('@') {
        push_basic_candidates(values, local_part);
    }
}

fn push_bracketed_mention_candidates(values: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<@")
        .and_then(|value| value.strip_suffix('>'))
    {
        let inner = inner.trim_start_matches('!').trim_start_matches('&');
        push_basic_candidates(values, inner);
    }
}

fn strip_channel_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    match trimmed.split_once(':') {
        Some((prefix, tail))
            if !tail.is_empty()
                && prefix
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') =>
        {
            tail
        }
        _ => trimmed,
    }
}

fn strip_slack_mention(raw: &str) -> &str {
    raw.trim()
        .strip_prefix("<@")
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(raw)
}

fn strip_teams_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    for prefix in ["8:orgid:", "8:", "29:", "orgid:"] {
        if let Some(stripped) = trimmed.strip_prefix(prefix) {
            return stripped;
        }
    }
    trimmed
}

fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !candidate.is_empty() && !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
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

    /// Helper: pair a device with a real Ed25519 public key.
    fn setup_with_ed25519() -> (IdentityManager, ring::signature::Ed25519KeyPair) {
        use ring::signature::{Ed25519KeyPair, KeyPair as _};

        let mgr = IdentityManager::new(":memory:").unwrap();
        let uid = UserId("bob".into());
        mgr.register_user(uid.clone(), "Bob".into(), UserRole::Owner)
            .unwrap();

        // Generate an Ed25519 keypair.
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();

        // Store the base64-encoded public key as `public_key_hash`.
        use base64::Engine;
        let pub_b64 =
            base64::engine::general_purpose::STANDARD.encode(key_pair.public_key().as_ref());

        let device = ngenorca_core::identity::DeviceBinding {
            device_id: DeviceId("dev-ed25519".into()),
            device_name: "Test Device".into(),
            attestation: ngenorca_core::identity::AttestationType::CompositeFingerprint,
            public_key_hash: pub_b64,
            trust: TrustLevel::Hardware,
            paired_at: chrono::Utc::now(),
            last_used: chrono::Utc::now(),
        };
        mgr.pair_device(&uid, device).unwrap();

        (mgr, key_pair)
    }

    #[test]
    fn resolve_from_known_device_no_signature_proceeds_reduced() {
        let mgr = setup();
        let result = resolve_from_device(&mgr, &DeviceId("dev-1".into()), &[], &[]).unwrap();
        assert_eq!(result.user_id, Some(UserId("alice".into())));
        assert_eq!(result.action, IdentityAction::ProceedReduced);
        // Trust capped at Channel (min of Hardware, Channel).
        assert_eq!(result.trust, TrustLevel::Channel);
    }

    #[test]
    fn resolve_unknown_device() {
        let mgr = setup();
        let result = resolve_from_device(&mgr, &DeviceId("unknown".into()), &[], &[]).unwrap();
        assert!(result.user_id.is_none());
        assert_eq!(result.action, IdentityAction::RequirePairing);
        assert_eq!(result.trust, TrustLevel::Unknown);
    }

    #[test]
    fn resolve_valid_ed25519_signature_proceeds() {
        let (mgr, key_pair) = setup_with_ed25519();
        let message = b"hello NgenOrca";
        let sig = key_pair.sign(message);

        let result =
            resolve_from_device(&mgr, &DeviceId("dev-ed25519".into()), sig.as_ref(), message)
                .unwrap();

        assert_eq!(result.user_id, Some(UserId("bob".into())));
        assert_eq!(result.trust, TrustLevel::Hardware);
        assert_eq!(result.action, IdentityAction::Proceed);
    }

    #[test]
    fn resolve_invalid_signature_challenges() {
        let (mgr, _key_pair) = setup_with_ed25519();
        let message = b"hello NgenOrca";
        let bad_sig = vec![0u8; 64]; // wrong signature

        let result =
            resolve_from_device(&mgr, &DeviceId("dev-ed25519".into()), &bad_sig, message).unwrap();

        assert_eq!(result.user_id, Some(UserId("bob".into())));
        assert_eq!(result.trust, TrustLevel::Unknown);
        assert_eq!(result.action, IdentityAction::Challenge);
    }

    #[test]
    fn resolve_corrupt_public_key_challenges() {
        let mgr = IdentityManager::new(":memory:").unwrap();
        let uid = UserId("carol".into());
        mgr.register_user(uid.clone(), "Carol".into(), UserRole::Owner)
            .unwrap();

        let device = ngenorca_core::identity::DeviceBinding {
            device_id: DeviceId("dev-bad-key".into()),
            device_name: "BadKey".into(),
            attestation: ngenorca_core::identity::AttestationType::CompositeFingerprint,
            public_key_hash: "not-valid-base64!!!".into(),
            trust: TrustLevel::Hardware,
            paired_at: chrono::Utc::now(),
            last_used: chrono::Utc::now(),
        };
        mgr.pair_device(&uid, device).unwrap();

        let result = resolve_from_device(
            &mgr,
            &DeviceId("dev-bad-key".into()),
            b"fake-sig",
            b"message",
        )
        .unwrap();

        assert_eq!(result.action, IdentityAction::Challenge);
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
    fn channel_handle_candidates_cover_common_alias_forms() {
        assert!(channel_handle_candidates(&ChannelKind::WhatsApp, "whatsapp:+1 (555) 010-0200")
            .contains(&"15550100200".to_string()));
        assert!(channel_handle_candidates(&ChannelKind::Slack, "<@U123ABC>")
            .contains(&"U123ABC".to_string()));
        assert!(channel_handle_candidates(&ChannelKind::Discord, "<@!998877>")
            .contains(&"998877".to_string()));
        assert!(channel_handle_candidates(&ChannelKind::Teams, "8:orgid:alice@example.com")
            .contains(&"alice".to_string()));
    }

    #[test]
    fn identity_action_equality() {
        assert_eq!(IdentityAction::Proceed, IdentityAction::Proceed);
        assert_ne!(IdentityAction::Proceed, IdentityAction::Block);
        assert_ne!(IdentityAction::RequirePairing, IdentityAction::Challenge);
    }

    #[test]
    fn verify_device_signature_valid() {
        use base64::Engine;
        use ring::signature::{Ed25519KeyPair, KeyPair as _};

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let pub_b64 =
            base64::engine::general_purpose::STANDARD.encode(key_pair.public_key().as_ref());

        let msg = b"test message";
        let sig = key_pair.sign(msg);

        assert!(verify_device_signature(&pub_b64, sig.as_ref(), msg).unwrap());
    }

    #[test]
    fn verify_device_signature_invalid() {
        use base64::Engine;
        use ring::signature::{Ed25519KeyPair, KeyPair as _};

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let pub_b64 =
            base64::engine::general_purpose::STANDARD.encode(key_pair.public_key().as_ref());

        let msg = b"test message";
        let wrong_sig = vec![0u8; 64];

        assert!(!verify_device_signature(&pub_b64, &wrong_sig, msg).unwrap());
    }
}
