//! QR-code pairing and credential exchange for WhatsApp Web multi-device.
//!
//! The pairing flow:
//! 1. Device generates Signal keys (identity, signed pre-key, pre-keys).
//! 2. Connects via WebSocket + Noise handshake.
//! 3. Receives a "ref" (challenge token) from the server.
//! 4. Composes QR code data: `ref,publicKey,clientId,timestamp`.
//! 5. User scans QR with their phone.
//! 6. Server sends `PairSuccessMessage` with signed device identity.
//! 7. Device stores credentials for future logins (no re-pairing needed).

use crate::binary::WaNode;
use crate::proto;
use crate::signal::SignalKeys;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// A 20-byte client ID that identifies this companion device.
pub fn generate_client_id() -> Vec<u8> {
    crate::crypto::random_bytes(16)
}

/// Credentials persisted after a successful pairing.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// The WhatsApp JID (phone@s.whatsapp.net).
    pub jid: String,
    /// Device identifier (e.g. `phone:device_id`).
    pub device_id: u16,
    /// Random client ID generated at pairing time.
    pub client_id: Vec<u8>,
    /// Noise static key pair (private, public) for reconnection.
    pub noise_private: Vec<u8>,
    pub noise_public: Vec<u8>,
    /// Signal Protocol keys.
    pub signal_keys: SignalKeys,
    /// ADV secret key for identity verification.
    pub adv_secret: Vec<u8>,
    /// Unix timestamp of pairing.
    pub paired_at: u64,
}

/// QR code data ready for display.
#[derive(Debug, Clone)]
pub struct QrCode {
    /// The raw data string: `ref,publicKey,clientId,timestamp`.
    pub data: String,
    /// Terminal-friendly ASCII art representation.
    pub terminal_string: String,
}

/// Compose QR code data from a server ref and our keys.
///
/// Format: `ref,base64(noisePublicKey),base64(identityPublicKey),base64(clientId),timestamp`
pub fn build_qr_data(
    server_ref: &str,
    noise_public: &[u8],
    identity_public: &[u8],
    client_id: &[u8],
) -> QrCode {
    let data = format!(
        "{},{},{},{}",
        server_ref,
        B64.encode(noise_public),
        B64.encode(identity_public),
        B64.encode(client_id),
    );

    let qr = qrcode::QrCode::new(data.as_bytes()).unwrap_or_else(|_| {
        // Fallback: create a minimal QR code.
        qrcode::QrCode::new(b"error").unwrap()
    });

    let terminal_string = qr
        .render::<char>()
        .quiet_zone(true)
        .module_dimensions(2, 1)
        .build();

    QrCode {
        data,
        terminal_string,
    }
}

/// Data extracted from the server's pairing success response.
#[derive(Debug, Clone)]
pub struct PairSuccess {
    pub jid: String,
    pub device_id: u16,
    pub platform: String,
    pub business_name: Option<String>,
}

/// Extract the QR "ref" from the server's response node.
///
/// After the Noise handshake completes, the server sends IQ nodes.
/// We look for the QR reference in the paired device flow.
pub fn extract_qr_ref(node: &WaNode) -> Option<String> {
    // The ref comes as a child node within an IQ response.
    // Different server versions send this differently.

    // Try direct content:
    if node.tag == "ref"
        && let crate::binary::WaNodeContent::Text(ref t) = node.content
    {
        return Some(t.clone());
    }

    // Try children:
    if let crate::binary::WaNodeContent::List(ref children) = node.content {
        for child in children {
            if child.tag == "ref"
                && let crate::binary::WaNodeContent::Text(ref t) = child.content
            {
                return Some(t.clone());
            }
            // Recurse one level deeper.
            if let crate::binary::WaNodeContent::List(ref grandchildren) = child.content {
                for gc in grandchildren {
                    if gc.tag == "ref"
                        && let crate::binary::WaNodeContent::Text(ref t) = gc.content
                    {
                        return Some(t.clone());
                    }
                }
            }
        }
    }

    None
}

/// Parse a pair-success WABinary node into structured data.
pub fn parse_pair_success(node: &WaNode) -> Option<PairSuccess> {
    if node.tag != "pair-success" && node.tag != "success" {
        return None;
    }

    // Extract device identity from child nodes.
    let children = match &node.content {
        crate::binary::WaNodeContent::List(c) => c,
        _ => return None,
    };

    let mut jid = None;
    let mut device_id = 0u16;
    let mut platform = String::from("unknown");
    let mut business_name = None;

    for child in children {
        match child.tag.as_str() {
            "device" => {
                jid = child.attr("jid").map(|s| s.to_string());
            }
            "device-identity" => {
                // Contains ADVSignedDeviceIdentity protobuf.
                debug!("pair-success: received device-identity");
            }
            "platform" => {
                if let Some(name) = child.attr("name") {
                    platform = name.to_string();
                }
            }
            "biz" => {
                business_name = child.attr("name").map(|s| s.to_string());
            }
            _ => {}
        }
    }

    // Fallback: get JID from the node's own attributes.
    if jid.is_none() {
        jid = node.attr("jid").map(|s| s.to_string());
    }

    // Parse device_id from JID suffix.
    if let Some(ref j) = jid
        && let Some(idx) = j.find(':')
        && let Ok(id) = j[idx + 1..].split('@').next().unwrap_or("0").parse::<u16>()
    {
        device_id = id;
    }

    jid.map(|j| {
        info!(jid = %j, device_id, platform = %platform, "Pair success");
        PairSuccess {
            jid: j,
            device_id,
            platform,
            business_name,
        }
    })
}

/// Build the IQ stanza that uploads our Signal pre-keys to the server.
pub fn build_prekey_upload_node(keys: &SignalKeys) -> WaNode {
    use crate::binary::WaNodeContent;
    use std::collections::HashMap;

    // Registration node.
    let reg_node = WaNode {
        tag: "registration".into(),
        attrs: HashMap::new(),
        content: WaNodeContent::Binary({
            let mut buf = Vec::with_capacity(4);
            buf.extend_from_slice(&keys.registration_id.to_be_bytes());
            buf
        }),
    };

    // Type node.
    let type_node = WaNode {
        tag: "type".into(),
        attrs: HashMap::new(),
        content: WaNodeContent::Binary(vec![0x05]), // Curve25519
    };

    // Identity key.
    let identity_node = WaNode {
        tag: "identity".into(),
        attrs: HashMap::new(),
        content: WaNodeContent::Binary(keys.identity.public.clone()),
    };

    // Signed pre-key.
    let skey_attrs = {
        let mut m = HashMap::new();
        m.insert("id".into(), keys.signed_prekey.id.to_string());
        m
    };
    let skey_node = WaNode {
        tag: "skey".into(),
        attrs: skey_attrs,
        content: WaNodeContent::List(vec![
            WaNode {
                tag: "value".into(),
                attrs: HashMap::new(),
                content: WaNodeContent::Binary(keys.signed_prekey.public.clone()),
            },
            WaNode {
                tag: "signature".into(),
                attrs: HashMap::new(),
                content: WaNodeContent::Binary(keys.signed_prekey.signature.clone()),
            },
        ]),
    };

    // Pre-keys list.
    let prekey_nodes: Vec<WaNode> = keys
        .prekeys
        .iter()
        .map(|pk| {
            let mut attrs = HashMap::new();
            attrs.insert("id".into(), pk.id.to_string());
            WaNode {
                tag: "key".into(),
                attrs,
                content: WaNodeContent::Binary(pk.public.clone()),
            }
        })
        .collect();

    let list_node = WaNode {
        tag: "list".into(),
        attrs: HashMap::new(),
        content: WaNodeContent::List(prekey_nodes),
    };

    // Wrap in an IQ stanza.
    let mut iq_attrs = HashMap::new();
    iq_attrs.insert("id".into(), uuid::Uuid::new_v4().to_string());
    iq_attrs.insert("xmlns".into(), "encrypt".into());
    iq_attrs.insert("type".into(), "set".into());
    iq_attrs.insert("to".into(), "s.whatsapp.net".into());

    WaNode {
        tag: "iq".into(),
        attrs: iq_attrs,
        content: WaNodeContent::List(vec![
            reg_node,
            type_node,
            identity_node,
            skey_node,
            list_node,
        ]),
    }
}

/// Build the `ClientPayload` protobuf for pairing (new device).
pub fn build_pairing_payload(keys: &SignalKeys) -> Vec<u8> {
    proto::build_client_payload_pairing(
        keys.registration_id,
        &keys.identity.public,
        &keys.signed_prekey.public,
        &keys.signed_prekey.signature,
        keys.signed_prekey.id,
    )
}

/// Build the `ClientPayload` protobuf for login (returning device).
pub fn build_login_payload(jid: &str) -> Vec<u8> {
    // Extract the phone number (username) from the JID.
    let username: u64 = jid
        .split('@')
        .next()
        .and_then(|s| s.split(':').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    proto::build_client_payload_login(username)
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_length() {
        let id = generate_client_id();
        assert_eq!(id.len(), 16);
    }

    #[test]
    fn qr_code_build() {
        let noise_pub = vec![0u8; 32];
        let ident_pub = vec![1u8; 32];
        let client_id = vec![2u8; 16];

        let qr = build_qr_data("test-ref-123", &noise_pub, &ident_pub, &client_id);
        assert!(qr.data.starts_with("test-ref-123,"));
        assert!(!qr.terminal_string.is_empty());
    }

    #[test]
    fn extract_qr_ref_direct() {
        let node = WaNode {
            tag: "ref".into(),
            attrs: std::collections::HashMap::new(),
            content: crate::binary::WaNodeContent::Text(
                "abc-ref-value".into(),
            ),
        };
        assert_eq!(extract_qr_ref(&node), Some("abc-ref-value".into()));
    }

    #[test]
    fn extract_qr_ref_nested() {
        let child = WaNode {
            tag: "ref".into(),
            attrs: std::collections::HashMap::new(),
            content: crate::binary::WaNodeContent::Text("nested-ref".into()),
        };
        let parent = WaNode {
            tag: "iq".into(),
            attrs: std::collections::HashMap::new(),
            content: crate::binary::WaNodeContent::List(vec![child]),
        };
        assert_eq!(extract_qr_ref(&parent), Some("nested-ref".into()));
    }

    #[test]
    fn prekey_upload_node_structure() {
        let keys = SignalKeys::generate();
        let node = build_prekey_upload_node(&keys);
        assert_eq!(node.tag, "iq");
        assert_eq!(node.attr("xmlns"), Some("encrypt"));
        assert_eq!(node.attr("type"), Some("set"));

        // Should have 5 children: registration, type, identity, skey, list.
        if let crate::binary::WaNodeContent::List(ref children) = node.content {
            assert_eq!(children.len(), 5);
            assert_eq!(children[0].tag, "registration");
            assert_eq!(children[1].tag, "type");
            assert_eq!(children[2].tag, "identity");
            assert_eq!(children[3].tag, "skey");
            assert_eq!(children[4].tag, "list");
        } else {
            panic!("Expected List content");
        }
    }

    #[test]
    fn pairing_payload_not_empty() {
        let keys = SignalKeys::generate();
        let payload = build_pairing_payload(&keys);
        assert!(!payload.is_empty());
    }

    #[test]
    fn login_payload_not_empty() {
        let payload = build_login_payload("1234567890@s.whatsapp.net");
        assert!(!payload.is_empty());
    }
}
