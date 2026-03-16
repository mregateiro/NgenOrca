//! Runtime identity resolution helpers.
//!
//! These helpers keep the live serving paths aligned so HTTP, WebSocket,
//! and inbound channel-worker flows can resolve the same canonical user
//! when channel bindings or device signatures exist.

use crate::auth::{AuthMethod, CallerIdentity};
use base64::Engine;
use ngenorca_core::message::{Content, Message};
use ngenorca_core::types::{ChannelId, ChannelKind, DeviceId, TrustLevel, UserId};
use ngenorca_identity::IdentityManager;
use ngenorca_identity::resolver::{IdentityAction, resolve_from_channel, resolve_from_device};

#[derive(Debug, Clone)]
pub struct RuntimeIdentity {
    pub user_id: Option<UserId>,
    pub trust: TrustLevel,
    pub action: IdentityAction,
    pub linked_handles: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeviceIdentityClaim {
    pub device_id: DeviceId,
    pub signature: Vec<u8>,
    pub message_bytes: Vec<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IdentityResolutionDiagnostics {
    pub action: String,
    pub trust: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub requires_pairing: bool,
    pub requires_challenge: bool,
    pub reason: String,
    #[serde(default)]
    pub suggested_actions: Vec<String>,
    #[serde(default)]
    pub matched_handles: Vec<String>,
    #[serde(default)]
    pub linked_handles: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_start_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_complete_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_start_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_verify_path: Option<String>,
}

pub fn resolve_web_identity(
    identity: &IdentityManager,
    caller: &CallerIdentity,
) -> RuntimeIdentity {
    resolve_web_identity_with_device(identity, caller, None)
}

pub fn resolve_web_identity_with_device(
    identity: &IdentityManager,
    caller: &CallerIdentity,
    device_claim: Option<&DeviceIdentityClaim>,
) -> RuntimeIdentity {
    let auth_trust = auth_trust(&caller.auth_method);

    if let Some(claim) = device_claim
        && let Ok(resolved) = resolve_from_device(
            identity,
            &claim.device_id,
            &claim.signature,
            &claim.message_bytes,
        )
    {
        match resolved.action {
            IdentityAction::Proceed | IdentityAction::ProceedReduced
                if resolved.user_id.is_some() =>
            {
                let user_id = resolved.user_id;
                let linked_handles = if matches!(resolved.action, IdentityAction::Proceed) {
                    user_id
                        .as_ref()
                        .map(|user_id| auto_link_web_handles(identity, user_id, caller))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                return RuntimeIdentity {
                    user_id,
                    trust: resolved.trust.max(auth_trust),
                    action: resolved.action,
                    linked_handles,
                };
            }
            IdentityAction::Challenge | IdentityAction::Block => {
                return RuntimeIdentity {
                    user_id: resolved.user_id,
                    trust: resolved.trust,
                    action: resolved.action,
                    linked_handles: Vec::new(),
                };
            }
            IdentityAction::Proceed
            | IdentityAction::ProceedReduced
            | IdentityAction::RequirePairing => {}
        }
    }

    for handle in web_handles(caller) {
        if let Ok(resolved) = resolve_from_channel(identity, &ChannelKind::WebChat, &handle)
            && matches!(
                resolved.action,
                IdentityAction::Proceed | IdentityAction::ProceedReduced
            )
            && resolved.user_id.is_some()
        {
            return RuntimeIdentity {
                user_id: resolved.user_id,
                trust: resolved.trust.max(auth_trust),
                action: resolved.action,
                linked_handles: Vec::new(),
            };
        }
    }

    RuntimeIdentity {
        user_id: caller
            .username
            .as_ref()
            .map(|username| UserId(username.clone())),
        trust: auth_trust,
        action: if caller.username.is_some() || caller.email.is_some() {
            IdentityAction::ProceedReduced
        } else {
            IdentityAction::RequirePairing
        },
        linked_handles: Vec::new(),
    }
}

pub fn resolve_message_identity(identity: &IdentityManager, message: &Message) -> RuntimeIdentity {
    if let Some(claim) = message_device_claim(message)
        && let Ok(resolved) = resolve_from_device(
            identity,
            &claim.device_id,
            &claim.signature,
            &claim.message_bytes,
        )
    {
        match resolved.action {
            IdentityAction::Proceed | IdentityAction::ProceedReduced
                if resolved.user_id.is_some() =>
            {
                let user_id = resolved.user_id;
                let linked_handles = if matches!(resolved.action, IdentityAction::Proceed) {
                    user_id
                        .as_ref()
                        .map(|user_id| auto_link_message_handle(identity, user_id, message))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                return RuntimeIdentity {
                    user_id,
                    trust: resolved.trust.max(message.trust),
                    action: resolved.action,
                    linked_handles,
                };
            }
            IdentityAction::Challenge | IdentityAction::Block => {
                return RuntimeIdentity {
                    user_id: resolved.user_id,
                    trust: resolved.trust,
                    action: resolved.action,
                    linked_handles: Vec::new(),
                };
            }
            IdentityAction::Proceed
            | IdentityAction::ProceedReduced
            | IdentityAction::RequirePairing => {}
        }
    }

    for handle in channel_handles(message) {
        if let Ok(resolved) = resolve_from_channel(identity, &message.channel_kind, &handle)
            && matches!(
                resolved.action,
                IdentityAction::Proceed | IdentityAction::ProceedReduced
            )
            && resolved.user_id.is_some()
        {
            return RuntimeIdentity {
                user_id: resolved.user_id,
                trust: resolved.trust.max(message.trust),
                action: resolved.action,
                linked_handles: Vec::new(),
            };
        }
    }

    RuntimeIdentity {
        user_id: message.user_id.clone(),
        trust: message.trust,
        action: if message.user_id.is_some() {
            IdentityAction::ProceedReduced
        } else {
            IdentityAction::RequirePairing
        },
        linked_handles: Vec::new(),
    }
}

pub fn build_web_device_claim(
    device_id: Option<&str>,
    device_signature: Option<&str>,
    message: &str,
) -> Option<DeviceIdentityClaim> {
    let device_id = device_id?.trim();
    let signature = decode_binary_claim(device_signature?)?;

    Some(DeviceIdentityClaim {
        device_id: DeviceId(device_id.to_string()),
        signature,
        message_bytes: message.as_bytes().to_vec(),
    })
}

pub fn describe_web_identity(
    caller: &CallerIdentity,
    resolved: &RuntimeIdentity,
    device_claim: Option<&DeviceIdentityClaim>,
) -> IdentityResolutionDiagnostics {
    let matched_handles = web_handles(caller);
    let mut suggested_actions = Vec::new();
    let reason = match resolved.action {
        IdentityAction::Proceed => "Identity resolved successfully.".to_string(),
        IdentityAction::ProceedReduced => {
            suggested_actions.push(
                "Link this authenticated handle or pair a trusted device to promote the session to full trust."
                    .into(),
            );
            if device_claim.is_none() {
                suggested_actions.push(
                    "Provide a signed device claim on future requests for hardware-backed verification."
                        .into(),
                );
            }
            "Identity is usable, but only at reduced trust because no canonical verified binding was confirmed for this request.".into()
        }
        IdentityAction::RequirePairing => {
            suggested_actions.push(
                "Pair a device or link this web identity to a canonical user before relying on stable memory and session continuity."
                    .into(),
            );
            suggested_actions.push(
                "Retry after pairing to unlock full-trust identity continuity across channels."
                    .into(),
            );
            if device_claim.is_some() {
                "This device is not paired to a canonical user yet.".into()
            } else {
                "No paired device or linked channel identity was available for this request.".into()
            }
        }
        IdentityAction::Challenge => {
            suggested_actions
                .push("Retry with a fresh signed payload from the paired device.".into());
            suggested_actions
                .push("If the device was rotated or reset, re-pair it before retrying.".into());
            "A known device claim was presented, but verification failed for this request.".into()
        }
        IdentityAction::Block => {
            suggested_actions.push(
                "Review the identity policy and repair the device or channel binding before retrying.".into(),
            );
            "Identity policy blocked this sender.".into()
        }
    };

    IdentityResolutionDiagnostics {
        action: identity_action_label(&resolved.action).into(),
        trust: trust_label(&resolved.trust).into(),
        user_id: resolved.user_id.as_ref().map(|value| value.0.clone()),
        requires_pairing: matches!(resolved.action, IdentityAction::RequirePairing),
        requires_challenge: matches!(
            resolved.action,
            IdentityAction::Challenge | IdentityAction::Block
        ),
        reason,
        suggested_actions,
        matched_handles,
        linked_handles: resolved.linked_handles.clone(),
        device_id: device_claim.map(|claim| claim.device_id.0.clone()),
        pairing_start_path: Some("/api/v1/identity/pairing/start".into()),
        pairing_complete_path: Some("/api/v1/identity/pairing/complete".into()),
        challenge_start_path: Some("/api/v1/identity/challenge/start".into()),
        challenge_verify_path: Some("/api/v1/identity/challenge/verify".into()),
    }
}

pub fn describe_message_identity(
    message: &Message,
    resolved: &RuntimeIdentity,
) -> IdentityResolutionDiagnostics {
    let matched_handles = channel_handles(message);
    let device_id = message_device_claim(message).map(|claim| claim.device_id.0);
    let mut suggested_actions = Vec::new();
    let reason = match resolved.action {
        IdentityAction::Proceed => "Identity resolved successfully.".to_string(),
        IdentityAction::ProceedReduced => {
            suggested_actions.push(
                "Link this channel handle or pair a trusted device to improve identity confidence on future messages."
                    .into(),
            );
            "Message can proceed, but only with reduced trust because no fully verified canonical identity was confirmed.".into()
        }
        IdentityAction::RequirePairing => {
            suggested_actions.push(
                format!(
                    "Link the {} sender handle to a canonical user before expecting stable identity continuity.",
                    message.channel_kind
                ),
            );
            suggested_actions.push(
                "Pair a device for higher-trust verification when the channel supports it.".into(),
            );
            "No linked channel handle or paired device was available for this inbound message."
                .into()
        }
        IdentityAction::Challenge => {
            suggested_actions.push(
                "Inspect the device signature payload and retry only after a fresh, valid signed message is available.".into(),
            );
            "Inbound device verification failed for this message.".into()
        }
        IdentityAction::Block => {
            suggested_actions.push(
                "Review the sender's identity bindings and trust policy before processing additional messages.".into(),
            );
            "Identity policy blocked this inbound sender.".into()
        }
    };

    IdentityResolutionDiagnostics {
        action: identity_action_label(&resolved.action).into(),
        trust: trust_label(&resolved.trust).into(),
        user_id: resolved.user_id.as_ref().map(|value| value.0.clone()),
        requires_pairing: matches!(resolved.action, IdentityAction::RequirePairing),
        requires_challenge: matches!(
            resolved.action,
            IdentityAction::Challenge | IdentityAction::Block
        ),
        reason,
        suggested_actions,
        matched_handles,
        linked_handles: resolved.linked_handles.clone(),
        device_id,
        pairing_start_path: Some("/api/v1/identity/pairing/start".into()),
        pairing_complete_path: Some("/api/v1/identity/pairing/complete".into()),
        challenge_start_path: Some("/api/v1/identity/challenge/start".into()),
        challenge_verify_path: Some("/api/v1/identity/challenge/verify".into()),
    }
}

fn auth_trust(method: &AuthMethod) -> TrustLevel {
    match method {
        AuthMethod::Certificate => TrustLevel::Certificate,
        AuthMethod::TrustedProxy | AuthMethod::Token | AuthMethod::Password => TrustLevel::Channel,
        AuthMethod::Anonymous => TrustLevel::Unknown,
    }
}

fn web_handles(caller: &CallerIdentity) -> Vec<String> {
    let mut handles = Vec::new();

    if let Some(username) = caller.username.as_ref().filter(|s| !s.trim().is_empty()) {
        push_web_handle_candidates(&mut handles, username);
    }

    if let Some(email) = caller.email.as_ref().filter(|s| !s.trim().is_empty()) {
        push_web_handle_candidates(&mut handles, email);
    }

    handles
}

pub fn authenticated_web_handles(caller: &CallerIdentity) -> Vec<String> {
    let mut handles = Vec::new();

    if let Some(username) = caller
        .username
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        push_unique(&mut handles, username.trim().to_string());
    }

    if let Some(email) = caller
        .email
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        push_unique(&mut handles, email.trim().to_string());
    }

    handles
}

fn channel_handles(message: &Message) -> Vec<String> {
    let mut handles = Vec::new();

    match &message.channel_kind {
        ChannelKind::Telegram => {
            if let Some(username) = metadata_string(&message.metadata, "telegram_username") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &username);
            }
            if let Some(user_id) = metadata_value_to_string(&message.metadata, "telegram_user_id") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &user_id);
            }
        }
        ChannelKind::WhatsApp => {
            if let Some(from) = metadata_string(&message.metadata, "whatsapp_from") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &from);
            }
        }
        ChannelKind::Slack => {
            if let Some(user) = metadata_string(&message.metadata, "slack_user") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &user);
            }
        }
        ChannelKind::Signal => {
            if let Some(source) = metadata_string(&message.metadata, "signal_source") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &source);
            }
        }
        ChannelKind::Matrix => {
            if let Some(sender) = metadata_string(&message.metadata, "matrix_sender") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &sender);
            }
        }
        ChannelKind::Discord => {
            if let Some(username) = metadata_string(&message.metadata, "discord_author_username") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &username);
            }
        }
        ChannelKind::Teams => {
            if let Some(name) = metadata_string(&message.metadata, "teams_from_name") {
                push_channel_handle_candidates(&mut handles, &message.channel_kind, &name);
            }
        }
        ChannelKind::WebChat
        | ChannelKind::IRC
        | ChannelKind::IMessage
        | ChannelKind::Custom(_) => {}
    }

    if let Some(user_id) = &message.user_id {
        push_channel_handle_candidates(&mut handles, &message.channel_kind, &user_id.0);
    }

    handles
}

fn message_device_claim(message: &Message) -> Option<DeviceIdentityClaim> {
    let device_id = metadata_string(&message.metadata, "device_id")?;
    let signature = metadata_string(&message.metadata, "device_signature")
        .or_else(|| metadata_string(&message.metadata, "device_signature_b64"))?;

    Some(DeviceIdentityClaim {
        device_id: DeviceId(device_id.trim().to_string()),
        signature: decode_binary_claim(&signature)?,
        message_bytes: metadata_string(&message.metadata, "device_signed_payload")
            .map(|payload| payload.into_bytes())
            .unwrap_or_else(|| message_payload_bytes(message)),
    })
}

fn message_payload_bytes(message: &Message) -> Vec<u8> {
    match &message.content {
        Content::Text(text) => text.as_bytes().to_vec(),
        Content::Image {
            caption: Some(caption),
            ..
        } => caption.as_bytes().to_vec(),
        Content::Audio {
            transcript: Some(transcript),
            ..
        } => transcript.as_bytes().to_vec(),
        other => serde_json::to_vec(other).unwrap_or_default(),
    }
}

fn decode_binary_claim(value: &str) -> Option<Vec<u8>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(trimmed)
                .ok()
        })
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE
                .decode(trimmed)
                .ok()
        })
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(trimmed)
                .ok()
        })
}

fn push_web_handle_candidates(values: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim();
    push_basic_candidates(values, trimmed);

    if let Some((local_part, _)) = trimmed.split_once('@') {
        push_basic_candidates(values, local_part);
    }
}

fn push_channel_handle_candidates(values: &mut Vec<String>, channel_kind: &ChannelKind, raw: &str) {
    let trimmed = raw.trim();
    push_basic_candidates(values, trimmed);

    let unprefixed = strip_channel_prefix(trimmed);
    push_basic_candidates(values, unprefixed);

    match channel_kind {
        ChannelKind::Telegram | ChannelKind::Discord => {
            push_username_candidates(values, unprefixed);
        }
        ChannelKind::WhatsApp | ChannelKind::Signal => {
            push_phone_candidates(values, unprefixed);
        }
        ChannelKind::Matrix => {
            push_matrix_candidates(values, unprefixed);
        }
        ChannelKind::WebChat => {
            push_web_handle_candidates(values, unprefixed);
        }
        ChannelKind::Slack
        | ChannelKind::Teams
        | ChannelKind::IRC
        | ChannelKind::IMessage
        | ChannelKind::Custom(_) => {}
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
    let digits: String = raw.chars().filter(|ch| ch.is_ascii_digit()).collect();
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

    if let Some((local_part, _server)) = trimmed.split_once(':') {
        push_username_candidates(values, local_part);
    } else {
        push_username_candidates(values, trimmed);
    }
}

fn strip_channel_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    match trimmed.split_once(':') {
        Some((prefix, tail))
            if prefix
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') =>
        {
            tail
        }
        _ => trimmed,
    }
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    metadata.get(key)?.as_str().map(|value| value.to_string())
}

fn metadata_value_to_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    let value = metadata.get(key)?;
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    if let Some(n) = value.as_i64() {
        return Some(n.to_string());
    }
    if let Some(n) = value.as_u64() {
        return Some(n.to_string());
    }
    None
}

fn push_unique(values: &mut Vec<String>, candidate: String) {
    if !candidate.is_empty() && !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

fn auto_link_web_handles(
    identity: &IdentityManager,
    user_id: &UserId,
    caller: &CallerIdentity,
) -> Vec<String> {
    authenticated_web_handles(caller)
        .into_iter()
        .filter(|handle| try_link_channel_handle(identity, user_id, &ChannelKind::WebChat, handle))
        .collect()
}

fn auto_link_message_handle(
    identity: &IdentityManager,
    user_id: &UserId,
    message: &Message,
) -> Vec<String> {
    message
        .user_id
        .as_ref()
        .map(|value| value.0.clone())
        .into_iter()
        .filter(|handle| try_link_channel_handle(identity, user_id, &message.channel_kind, handle))
        .collect()
}

pub fn link_authenticated_web_handles(
    identity: &IdentityManager,
    user_id: &UserId,
    caller: &CallerIdentity,
) -> Vec<String> {
    auto_link_web_handles(identity, user_id, caller)
}

fn try_link_channel_handle(
    identity: &IdentityManager,
    user_id: &UserId,
    channel_kind: &ChannelKind,
    handle: &str,
) -> bool {
    let handle = handle.trim();
    if handle.is_empty() {
        return false;
    }

    match identity.resolve_by_channel(channel_kind, handle) {
        Ok(Some((resolved_user, _))) if resolved_user == *user_id => false,
        Ok(Some((_resolved_user, _))) => false,
        Ok(None) => identity
            .link_channel(
                user_id,
                ChannelId(format!("{channel_kind}:{handle}")),
                channel_kind.clone(),
                handle.to_string(),
            )
            .map(|_| true)
            .unwrap_or(false),
        Err(_) => false,
    }
}

fn identity_action_label(action: &IdentityAction) -> &'static str {
    match action {
        IdentityAction::Proceed => "proceed",
        IdentityAction::ProceedReduced => "proceed_reduced",
        IdentityAction::RequirePairing => "require_pairing",
        IdentityAction::Challenge => "challenge",
        IdentityAction::Block => "block",
    }
}

fn trust_label(trust: &TrustLevel) -> &'static str {
    match trust {
        TrustLevel::Unknown => "unknown",
        TrustLevel::Channel => "channel",
        TrustLevel::Certificate => "certificate",
        TrustLevel::Hardware => "hardware",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::identity::UserRole;
    use ngenorca_core::message::{Content, Direction, Message};
    use ngenorca_core::types::{ChannelId, EventId, SessionId};

    fn identity_manager() -> IdentityManager {
        IdentityManager::new(":memory:").unwrap()
    }

    fn sample_message(channel_kind: ChannelKind, user_id: Option<UserId>) -> Message {
        Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id,
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId("channel-1".into()),
            channel_kind,
            direction: Direction::Inbound,
            content: Content::Text("hello".into()),
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn resolve_web_identity_prefers_canonical_webchat_binding() {
        let manager = identity_manager();
        let canonical = UserId("owner-alice".into());
        manager
            .register_user(canonical.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();
        manager
            .link_channel(
                &canonical,
                ChannelId("webchat".into()),
                ChannelKind::WebChat,
                "alice".into(),
            )
            .unwrap();

        let caller = CallerIdentity {
            username: Some("alice".into()),
            email: None,
            groups: vec![],
            auth_method: AuthMethod::TrustedProxy,
        };

        let resolved = resolve_web_identity(&manager, &caller);
        assert_eq!(resolved.user_id, Some(canonical));
        assert_eq!(resolved.trust, TrustLevel::Channel);
        assert_eq!(resolved.action, IdentityAction::Proceed);
    }

    #[test]
    fn resolve_web_identity_falls_back_to_authenticated_username() {
        let manager = identity_manager();
        let caller = CallerIdentity {
            username: Some("proxy-user".into()),
            email: None,
            groups: vec![],
            auth_method: AuthMethod::TrustedProxy,
        };

        let resolved = resolve_web_identity(&manager, &caller);
        assert_eq!(resolved.user_id, Some(UserId("proxy-user".into())));
        assert_eq!(resolved.trust, TrustLevel::Channel);
        assert_eq!(resolved.action, IdentityAction::ProceedReduced);
    }

    #[test]
    fn resolve_message_identity_uses_channel_binding() {
        let manager = identity_manager();
        let canonical = UserId("alice".into());
        manager
            .register_user(canonical.clone(), "Alice".into(), UserRole::Owner)
            .unwrap();
        manager
            .link_channel(
                &canonical,
                ChannelId("tg-1".into()),
                ChannelKind::Telegram,
                "42".into(),
            )
            .unwrap();

        let mut message = sample_message(ChannelKind::Telegram, Some(UserId("telegram:42".into())));
        message.metadata = serde_json::json!({
            "telegram_user_id": 42,
            "telegram_username": "alice_dev"
        });

        let resolved = resolve_message_identity(&manager, &message);
        assert_eq!(resolved.user_id, Some(canonical));
        assert_eq!(resolved.trust, TrustLevel::Channel);
        assert_eq!(resolved.action, IdentityAction::Proceed);
    }

    #[test]
    fn resolve_message_identity_keeps_original_when_unbound() {
        let manager = identity_manager();
        let message = sample_message(ChannelKind::Slack, Some(UserId("slack:U123".into())));

        let resolved = resolve_message_identity(&manager, &message);
        assert_eq!(resolved.user_id, Some(UserId("slack:U123".into())));
        assert_eq!(resolved.trust, TrustLevel::Channel);
        assert_eq!(resolved.action, IdentityAction::ProceedReduced);
    }

    fn setup_with_ed25519() -> (IdentityManager, ring::signature::Ed25519KeyPair, UserId) {
        use base64::Engine;
        use ring::signature::{Ed25519KeyPair, KeyPair as _};

        let manager = identity_manager();
        let canonical = UserId("device-owner".into());
        manager
            .register_user(canonical.clone(), "Device Owner".into(), UserRole::Owner)
            .unwrap();

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public_key =
            base64::engine::general_purpose::STANDARD.encode(key_pair.public_key().as_ref());

        manager
            .pair_device(
                &canonical,
                ngenorca_core::identity::DeviceBinding {
                    device_id: DeviceId("dev-webchat".into()),
                    device_name: "Primary laptop".into(),
                    attestation: ngenorca_core::identity::AttestationType::CompositeFingerprint,
                    public_key_hash: public_key,
                    trust: TrustLevel::Hardware,
                    paired_at: chrono::Utc::now(),
                    last_used: chrono::Utc::now(),
                },
            )
            .unwrap();

        (manager, key_pair, canonical)
    }

    #[test]
    fn resolve_web_identity_prefers_valid_device_signature() {
        let (manager, key_pair, canonical) = setup_with_ed25519();
        let message = "signed hello";
        let signature = base64::engine::general_purpose::STANDARD
            .encode(key_pair.sign(message.as_bytes()).as_ref());
        let caller = CallerIdentity {
            username: Some("fallback-user".into()),
            email: None,
            groups: vec![],
            auth_method: AuthMethod::TrustedProxy,
        };
        let claim = build_web_device_claim(Some("dev-webchat"), Some(&signature), message).unwrap();

        let resolved = resolve_web_identity_with_device(&manager, &caller, Some(&claim));
        assert_eq!(resolved.user_id, Some(canonical.clone()));
        assert_eq!(resolved.trust, TrustLevel::Hardware);
        assert_eq!(resolved.action, IdentityAction::Proceed);
        assert_eq!(resolved.linked_handles, vec!["fallback-user".to_string()]);
        assert_eq!(
            manager
                .resolve_by_channel(&ChannelKind::WebChat, "fallback-user")
                .unwrap()
                .unwrap()
                .0,
            canonical
        );
    }

    #[test]
    fn resolve_message_identity_challenges_invalid_device_signature() {
        let (manager, _key_pair, canonical) = setup_with_ed25519();
        let mut message = sample_message(ChannelKind::Slack, Some(UserId("slack:U123".into())));
        message.metadata = serde_json::json!({
            "device_id": "dev-webchat",
            "device_signature": base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
            "device_signed_payload": "hello"
        });
        message.content = Content::Text("hello".into());

        let resolved = resolve_message_identity(&manager, &message);
        assert_eq!(resolved.user_id, Some(canonical));
        assert_eq!(resolved.trust, TrustLevel::Unknown);
        assert_eq!(resolved.action, IdentityAction::Challenge);
    }

    #[test]
    fn resolve_message_identity_normalizes_telegram_username_variants() {
        let manager = identity_manager();
        let canonical = UserId("telegram-owner".into());
        manager
            .register_user(canonical.clone(), "Telegram Owner".into(), UserRole::Owner)
            .unwrap();
        manager
            .link_channel(
                &canonical,
                ChannelId("tg-1".into()),
                ChannelKind::Telegram,
                "alice_dev".into(),
            )
            .unwrap();

        let mut message = sample_message(ChannelKind::Telegram, None);
        message.metadata = serde_json::json!({
            "telegram_username": "@Alice_Dev"
        });

        let resolved = resolve_message_identity(&manager, &message);
        assert_eq!(resolved.user_id, Some(canonical));
        assert_eq!(resolved.action, IdentityAction::Proceed);
    }

    #[test]
    fn resolve_message_identity_normalizes_matrix_sender_variants() {
        let manager = identity_manager();
        let canonical = UserId("matrix-owner".into());
        manager
            .register_user(canonical.clone(), "Matrix Owner".into(), UserRole::Owner)
            .unwrap();
        manager
            .link_channel(
                &canonical,
                ChannelId("mx-1".into()),
                ChannelKind::Matrix,
                "alice".into(),
            )
            .unwrap();

        let mut message = sample_message(
            ChannelKind::Matrix,
            Some(UserId("matrix:@alice:matrix.example".into())),
        );
        message.metadata = serde_json::json!({
            "matrix_sender": "@alice:matrix.example"
        });

        let resolved = resolve_message_identity(&manager, &message);
        assert_eq!(resolved.user_id, Some(canonical));
        assert_eq!(resolved.action, IdentityAction::Proceed);
    }

    #[test]
    fn resolve_message_identity_auto_links_verified_channel_alias() {
        use base64::Engine;

        let (manager, key_pair, canonical) = setup_with_ed25519();
        let mut message = sample_message(ChannelKind::Telegram, Some(UserId("telegram:42".into())));
        message.metadata = serde_json::json!({
            "device_id": "dev-webchat",
            "device_signature": base64::engine::general_purpose::STANDARD.encode(key_pair.sign(b"hello").as_ref()),
            "device_signed_payload": "hello",
            "telegram_user_id": 42,
        });
        message.content = Content::Text("hello".into());

        let resolved = resolve_message_identity(&manager, &message);
        assert_eq!(resolved.user_id, Some(canonical.clone()));
        assert_eq!(resolved.action, IdentityAction::Proceed);
        assert_eq!(resolved.linked_handles, vec!["telegram:42".to_string()]);
        assert_eq!(
            manager
                .resolve_by_channel(&ChannelKind::Telegram, "42")
                .unwrap()
                .unwrap()
                .0,
            canonical
        );
    }

    #[test]
    fn resolve_message_identity_normalizes_slack_and_discord_alias_forms() {
        let manager = identity_manager();
        let slack_user = UserId("slack-owner".into());
        manager
            .register_user(slack_user.clone(), "Slack Owner".into(), UserRole::Owner)
            .unwrap();
        manager
            .link_channel(
                &slack_user,
                ChannelId("slack-u123".into()),
                ChannelKind::Slack,
                "U123ABC".into(),
            )
            .unwrap();

        let mut slack_message = sample_message(ChannelKind::Slack, None);
        slack_message.metadata = serde_json::json!({
            "slack_user": "<@U123ABC>"
        });
        let slack_resolved = resolve_message_identity(&manager, &slack_message);
        assert_eq!(slack_resolved.user_id, Some(slack_user));

        let discord_user = UserId("discord-owner".into());
        manager
            .register_user(
                discord_user.clone(),
                "Discord Owner".into(),
                UserRole::Family,
            )
            .unwrap();
        manager
            .link_channel(
                &discord_user,
                ChannelId("discord-998877".into()),
                ChannelKind::Discord,
                "998877".into(),
            )
            .unwrap();

        let mut discord_message = sample_message(ChannelKind::Discord, None);
        discord_message.metadata = serde_json::json!({
            "discord_author_username": "<@!998877>"
        });
        let discord_resolved = resolve_message_identity(&manager, &discord_message);
        assert_eq!(discord_resolved.user_id, Some(discord_user));
    }
}
