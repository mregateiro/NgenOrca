//! Protobuf message definitions for the WhatsApp Web protocol.
//!
//! These are manually defined using `prost` attributes (no `.proto` build
//! step needed).  Only the subset required for text messaging is included.

use prost::Message;

// ─── Handshake Messages ─────────────────────────────────────────

/// Top-level handshake wrapper.
#[derive(Clone, PartialEq, Message)]
pub struct HandshakeMessage {
    #[prost(message, optional, tag = "2")]
    pub client_hello: Option<ClientHello>,
    #[prost(message, optional, tag = "3")]
    pub server_hello: Option<ServerHello>,
    #[prost(message, optional, tag = "4")]
    pub client_finish: Option<ClientFinish>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ClientHello {
    /// Our ephemeral public key (32 bytes).
    #[prost(bytes = "vec", optional, tag = "1")]
    pub ephemeral: Option<Vec<u8>>,
    /// Static key (encrypted during handshake).
    #[prost(bytes = "vec", optional, tag = "2")]
    pub r#static: Option<Vec<u8>>,
    /// Payload (encrypted ClientPayload).
    #[prost(bytes = "vec", optional, tag = "3")]
    pub payload: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ServerHello {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub ephemeral: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub r#static: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub payload: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ClientFinish {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub r#static: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub payload: Option<Vec<u8>>,
}

// ─── Authentication ─────────────────────────────────────────────

/// Client payload sent during the Noise handshake's client-finish step.
#[derive(Clone, PartialEq, Message)]
pub struct ClientPayload {
    /// Username (phone number as u64).
    #[prost(uint64, optional, tag = "1")]
    pub username: Option<u64>,
    /// Whether this is a passive reconnection.
    #[prost(bool, optional, tag = "3")]
    pub passive: Option<bool>,
    /// User agent information.
    #[prost(message, optional, tag = "5")]
    pub user_agent: Option<UserAgent>,
    /// Web-specific info.
    #[prost(message, optional, tag = "6")]
    pub web_info: Option<WebInfo>,
    /// Push notification token.
    #[prost(string, optional, tag = "7")]
    pub push_name: Option<String>,
    /// Device pairing data (for QR code pairing).
    #[prost(message, optional, tag = "35")]
    pub device_pairing_data: Option<DevicePairingData>,
}

#[derive(Clone, PartialEq, Message)]
pub struct UserAgent {
    #[prost(enumeration = "Platform", optional, tag = "1")]
    pub platform: Option<i32>,
    #[prost(message, optional, tag = "2")]
    pub app_version: Option<AppVersion>,
    #[prost(string, optional, tag = "5")]
    pub os_version: Option<String>,
    #[prost(string, optional, tag = "6")]
    pub device: Option<String>,
    #[prost(string, optional, tag = "7")]
    pub manufacturer: Option<String>,
    #[prost(enumeration = "ReleaseChannel", optional, tag = "4")]
    pub release_channel: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AppVersion {
    #[prost(uint32, optional, tag = "1")]
    pub primary: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub secondary: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub tertiary: Option<u32>,
    #[prost(uint32, optional, tag = "4")]
    pub quaternary: Option<u32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct WebInfo {
    #[prost(enumeration = "WebSubPlatform", optional, tag = "1")]
    pub web_sub_platform: Option<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct DevicePairingData {
    /// ERegId (registration id for Signal Protocol).
    #[prost(bytes = "vec", optional, tag = "1")]
    pub e_reg_id: Option<Vec<u8>>,
    /// Identity key (public).
    #[prost(bytes = "vec", optional, tag = "2")]
    pub e_key_type: Option<Vec<u8>>,
    /// Identity key (encoded).
    #[prost(bytes = "vec", optional, tag = "3")]
    pub e_ident: Option<Vec<u8>>,
    /// Signed pre-key id.
    #[prost(bytes = "vec", optional, tag = "4")]
    pub e_skey_id: Option<Vec<u8>>,
    /// Signed pre-key (public).
    #[prost(bytes = "vec", optional, tag = "5")]
    pub e_skey_val: Option<Vec<u8>>,
    /// Signed pre-key signature.
    #[prost(bytes = "vec", optional, tag = "6")]
    pub e_skey_sig: Option<Vec<u8>>,
    /// Build hash.
    #[prost(bytes = "vec", optional, tag = "7")]
    pub build_hash: Option<Vec<u8>>,
    /// Companion props.
    #[prost(bytes = "vec", optional, tag = "8")]
    pub companion_props: Option<Vec<u8>>,
}

// ─── Enums ──────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum Platform {
    Android = 0,
    Ios = 1,
    WindowsPhone = 2,
    Blackberry = 3,
    Blackberry10 = 4,
    S40 = 5,
    S60 = 6,
    Python = 7,
    Tizen = 8,
    Enterprise = 9,
    SmbaAndroid = 10,
    SmbaIos = 11,
    KaiOs = 12,
    SmbaWin = 13,
    Windows = 14,
    Web = 15,
    Portal = 16,
    GreenAndroid = 17,
    GreenIphone = 18,
    BlueMsyaAndroid = 19,
    BlueMsyaIphone = 20,
    Ohana = 21,
    Aloha = 22,
    Catalyst = 23,
    VrAndroid = 24,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum ReleaseChannel {
    Release = 0,
    Beta = 1,
    Alpha = 2,
    Debug = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, prost::Enumeration)]
#[repr(i32)]
pub enum WebSubPlatform {
    WebBrowser = 0,
    AppStore = 1,
    WinStore = 2,
    Darwin = 3,
    Win32 = 4,
}

// ─── WhatsApp Message Types ─────────────────────────────────────

/// A WhatsApp chat message (inner envelope, after Signal decryption).
#[derive(Clone, PartialEq, Message)]
pub struct WaMessage {
    /// Plain text conversation message.
    #[prost(string, optional, tag = "1")]
    pub conversation: Option<String>,
    /// Extended text message (with mentions, links, etc.).
    #[prost(message, optional, tag = "6")]
    pub extended_text_message: Option<ExtendedTextMessage>,
    // Images, video, audio, etc. are higher tag numbers — omitted for now.
}

#[derive(Clone, PartialEq, Message)]
pub struct ExtendedTextMessage {
    #[prost(string, optional, tag = "1")]
    pub text: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub matched_text: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub canonical_url: Option<String>,
    #[prost(string, optional, tag = "5")]
    pub description: Option<String>,
    #[prost(string, optional, tag = "6")]
    pub title: Option<String>,
}

// ─── ADV (Authenticated Device Verification) ────────────────────

/// Signed device identity received after QR pairing.
#[derive(Clone, PartialEq, Message)]
pub struct ADVSignedDeviceIdentity {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub details: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub account_signature: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub account_signature_key: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub device_signature: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ADVDeviceIdentity {
    #[prost(uint32, optional, tag = "1")]
    pub raw_id: Option<u32>,
    #[prost(uint64, optional, tag = "2")]
    pub timestamp: Option<u64>,
    #[prost(uint32, optional, tag = "3")]
    pub key_index: Option<u32>,
}

// ─── Encoding / Decoding Helpers ────────────────────────────────

/// Encode a client hello for the Noise handshake.
pub fn encode_handshake_client_hello(ephemeral_key: &[u8]) -> Vec<u8> {
    let msg = HandshakeMessage {
        client_hello: Some(ClientHello {
            ephemeral: Some(ephemeral_key.to_vec()),
            r#static: None,
            payload: None,
        }),
        server_hello: None,
        client_finish: None,
    };
    msg.encode_to_vec()
}

/// Decode a server hello from the Noise handshake.
///
/// Returns `(ephemeral_key, static_encrypted + payload_encrypted)`.
pub fn decode_handshake_server_hello(data: &[u8]) -> crate::Result<(Vec<u8>, Vec<u8>)> {
    let msg = HandshakeMessage::decode(data)
        .map_err(|e| crate::Error::Proto(format!("decode server hello: {e}")))?;

    let server = msg
        .server_hello
        .ok_or_else(|| crate::Error::Proto("no server_hello in handshake".into()))?;

    let ephemeral = server
        .ephemeral
        .ok_or_else(|| crate::Error::Proto("no ephemeral in server_hello".into()))?;

    // Concatenate static + payload (both encrypted).
    let mut encrypted = Vec::new();
    if let Some(s) = &server.r#static {
        encrypted.extend_from_slice(s);
    }
    if let Some(p) = &server.payload {
        encrypted.extend_from_slice(p);
    }

    Ok((ephemeral, encrypted))
}

/// Encode the client finish handshake message.
pub fn encode_handshake_client_finish(finish_data: &[u8]) -> Vec<u8> {
    let msg = HandshakeMessage {
        client_hello: None,
        server_hello: None,
        client_finish: Some(ClientFinish {
            r#static: Some(finish_data.to_vec()),
            payload: None,
        }),
    };
    msg.encode_to_vec()
}

/// Build a ClientPayload for initial device pairing.
pub fn build_client_payload_pairing(
    reg_id: u32,
    identity_key_public: &[u8],
    signed_prekey_public: &[u8],
    signed_prekey_sig: &[u8],
    signed_prekey_id: u32,
) -> Vec<u8> {
    let payload = ClientPayload {
        username: None, // Set after pairing
        passive: Some(false),
        user_agent: Some(UserAgent {
            platform: Some(Platform::Web as i32),
            app_version: Some(AppVersion {
                primary: Some(2),
                secondary: Some(2400),
                tertiary: Some(0),
                quaternary: Some(0),
            }),
            os_version: Some("NgenOrca".into()),
            device: Some("Desktop".into()),
            manufacturer: Some("NgenOrca".into()),
            release_channel: Some(ReleaseChannel::Release as i32),
        }),
        web_info: Some(WebInfo {
            web_sub_platform: Some(WebSubPlatform::WebBrowser as i32),
        }),
        push_name: Some("NgenOrca".into()),
        device_pairing_data: Some(DevicePairingData {
            e_reg_id: Some(reg_id.to_be_bytes().to_vec()),
            e_key_type: Some(vec![5]), // Curve25519
            e_ident: Some(identity_key_public.to_vec()),
            e_skey_id: Some(signed_prekey_id.to_be_bytes()[1..].to_vec()), // 3 bytes
            e_skey_val: Some(signed_prekey_public.to_vec()),
            e_skey_sig: Some(signed_prekey_sig.to_vec()),
            build_hash: None,
            companion_props: None,
        }),
    };
    payload.encode_to_vec()
}

/// Build a ClientPayload for session resumption (re-login).
pub fn build_client_payload_login(username: u64) -> Vec<u8> {
    let payload = ClientPayload {
        username: Some(username),
        passive: Some(true),
        user_agent: Some(UserAgent {
            platform: Some(Platform::Web as i32),
            app_version: Some(AppVersion {
                primary: Some(2),
                secondary: Some(2400),
                tertiary: Some(0),
                quaternary: Some(0),
            }),
            os_version: Some("NgenOrca".into()),
            device: Some("Desktop".into()),
            manufacturer: Some("NgenOrca".into()),
            release_channel: Some(ReleaseChannel::Release as i32),
        }),
        web_info: Some(WebInfo {
            web_sub_platform: Some(WebSubPlatform::WebBrowser as i32),
        }),
        push_name: Some("NgenOrca".into()),
        device_pairing_data: None,
    };
    payload.encode_to_vec()
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_client_hello_roundtrip() {
        let key = [0x42u8; 32];
        let encoded = encode_handshake_client_hello(&key);
        let decoded = HandshakeMessage::decode(encoded.as_slice()).unwrap();
        assert_eq!(
            decoded.client_hello.unwrap().ephemeral.unwrap(),
            key.to_vec()
        );
    }

    #[test]
    fn client_payload_pairing_encodes() {
        let identity = [0xAA; 32];
        let prekey = [0xBB; 32];
        let sig = [0xCC; 64];
        let encoded = build_client_payload_pairing(1234, &identity, &prekey, &sig, 1);
        let decoded = ClientPayload::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.passive, Some(false));
        assert!(decoded.device_pairing_data.is_some());
    }

    #[test]
    fn client_payload_login_encodes() {
        let encoded = build_client_payload_login(5511999999999);
        let decoded = ClientPayload::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.username, Some(5511999999999));
        assert_eq!(decoded.passive, Some(true));
    }

    #[test]
    fn wa_message_conversation_roundtrip() {
        let msg = WaMessage {
            conversation: Some("Hello!".into()),
            extended_text_message: None,
        };
        let encoded = msg.encode_to_vec();
        let decoded = WaMessage::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.conversation, Some("Hello!".into()));
    }
}
