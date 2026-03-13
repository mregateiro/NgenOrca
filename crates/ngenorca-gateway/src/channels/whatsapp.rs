//! WhatsApp adapter — dual-mode: Native (pure Rust) or Cloud API.
//!
//! ## Mode 1 — Native (default, no external deps)
//! Uses the `ngenorca-whatsapp-web` crate to implement the WhatsApp Web
//! multi-device protocol directly in Rust.  No Node.js, no webhook URL.
//! Authentication happens via QR-code scan (first run) with session data
//! persisted to `data_dir`.
//!
//! ## Mode 2 — Cloud API (legacy, requires public webhook URL)
//! Uses Meta's WhatsApp Business Cloud API for sending and receiving
//! messages.  Requires `access_token` and `phone_number_id` in config.
//!
//! The adapter auto-detects the mode based on config:
//! - If `access_token` is set → Cloud API mode
//! - Otherwise → Native mode (pure Rust, zero external runtime deps)

use async_trait::async_trait;
use ngenorca_core::ChannelKind;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{API_VERSION, Permission, PluginKind, PluginManifest};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext, flume_like};
use ngenorca_whatsapp_web::{WhatsAppClient, WhatsAppEvent};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use hmac::{Hmac, Mac};
use sha2::Sha256;
type HmacSha256 = Hmac<Sha256>;

const GRAPH_API_BASE: &str = "https://graph.facebook.com/v21.0";

/// Simple hex decode (avoids pulling in a hex crate).
fn hex_decode(hex: &str) -> std::result::Result<Vec<u8>, ()> {
    if !hex.len().is_multiple_of(2) {
        return Err(());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

/// Operational mode for the WhatsApp adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatsAppMode {
    /// Native pure-Rust mode — WhatsApp Web protocol implemented directly.
    /// No Node.js or external runtime needed.
    Native { data_dir: PathBuf },
    /// Cloud API — Meta's official REST + webhook approach.
    CloudApi {
        phone_number_id: String,
        access_token: String,
        verify_token: String,
        webhook_path: String,
        /// App secret for webhook signature verification (X-Hub-Signature-256).
        app_secret: Option<String>,
    },
}

/// WhatsApp adapter supporting Native (pure Rust) and Cloud API modes.
pub struct WhatsAppAdapter {
    mode: WhatsAppMode,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    client: reqwest::Client,
    /// Native WhatsApp Web client (pure Rust).
    wa_client: Arc<Mutex<Option<WhatsAppClient>>>,
}

impl WhatsAppAdapter {
    /// Create a new WhatsApp adapter in the given mode.
    pub fn new(mode: WhatsAppMode) -> Self {
        Self {
            mode,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::new(),
            wa_client: Arc::new(Mutex::new(None)),
        }
    }

    /// Convenience constructor for Native mode (pure Rust, no Node.js).
    pub fn native(data_dir: PathBuf) -> Self {
        Self::new(WhatsAppMode::Native { data_dir })
    }

    /// Convenience constructor for Cloud API mode.
    pub fn cloud_api(
        phone_number_id: String,
        access_token: String,
        verify_token: String,
        webhook_path: String,
        app_secret: Option<String>,
    ) -> Self {
        Self::new(WhatsAppMode::CloudApi {
            phone_number_id,
            access_token,
            verify_token,
            webhook_path,
            app_secret,
        })
    }

    // ── Native mode helpers ─────────────────────────────────────

    /// Send a text message via the native WhatsApp Web client.
    async fn send_text_native(&self, to: &str, text: &str) -> ngenorca_core::Result<()> {
        let guard = self.wa_client.lock().await;
        let wa = guard.as_ref().ok_or_else(|| {
            ngenorca_core::Error::Other("WhatsApp native client not connected".into())
        })?;

        // Ensure JID format: `number@s.whatsapp.net`
        let jid = if to.contains('@') {
            to.to_string()
        } else {
            format!("{to}@s.whatsapp.net")
        };

        wa.send_text(&jid, text)
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("WhatsApp send: {e}")))?;

        debug!(to = to, "WhatsApp message sent via native client");
        Ok(())
    }

    /// Convert a native `WhatsAppEvent::Message` into an NgenOrca `Message`.
    pub(crate) fn native_event_to_ngenorca(
        from: &str,
        text: &str,
        timestamp: u64,
        message_id: &str,
    ) -> Message {
        let phone = from.split('@').next().unwrap_or(from);
        let channel = from.split('@').next().unwrap_or(from);

        Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId(format!("whatsapp:{phone}"))),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId(channel.to_string()),
            channel_kind: ChannelKind::WhatsApp,
            direction: Direction::Inbound,
            content: Content::Text(text.to_string()),
            metadata: serde_json::json!({
                "whatsapp_from": from,
                "whatsapp_message_id": message_id,
                "whatsapp_timestamp": timestamp,
            }),
        }
    }

    // ── Cloud API helpers (legacy) ──────────────────────────────

    /// Send a text message via the Cloud API.
    async fn send_text_cloud(
        &self,
        phone_number_id: &str,
        access_token: &str,
        to: &str,
        text: &str,
    ) -> ngenorca_core::Result<()> {
        let url = format!("{GRAPH_API_BASE}/{phone_number_id}/messages");

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": to,
            "type": "text",
            "text": {
                "preview_url": false,
                "body": text
            }
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("WhatsApp send: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(ngenorca_core::Error::Other(format!(
                "WhatsApp send failed ({status}): {err}"
            )));
        }

        debug!(to = to, "WhatsApp message sent via Cloud API");
        Ok(())
    }

    /// Convert a Cloud API webhook message into an NgenOrca Message.
    #[allow(dead_code)]
    pub(crate) fn webhook_message_to_ngenorca(
        msg: &WaMessage,
        contact_name: Option<&str>,
    ) -> Option<Message> {
        let text = msg.text.as_ref()?.body.as_str();
        if text.is_empty() {
            return None;
        }

        Some(Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId(format!("whatsapp:{}", msg.from))),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId(msg.from.clone()),
            channel_kind: ChannelKind::WhatsApp,
            direction: Direction::Inbound,
            content: Content::Text(text.to_string()),
            metadata: serde_json::json!({
                "whatsapp_from": msg.from,
                "whatsapp_message_id": msg.id,
                "whatsapp_contact_name": contact_name,
                "whatsapp_timestamp": msg.timestamp,
            }),
        })
    }

    /// Process a Cloud API webhook notification payload.
    #[allow(dead_code)]
    pub(crate) fn process_webhook_payload(payload: &WaWebhookPayload, sender: &flume_like::Sender) {
        for entry in &payload.entry {
            for change in &entry.changes {
                if change.field != "messages" {
                    continue;
                }

                let value = &change.value;

                let contact_name = value
                    .contacts
                    .as_ref()
                    .and_then(|c| c.first())
                    .and_then(|c| c.profile.as_ref())
                    .map(|p| p.name.as_str());

                for msg in value.messages.as_deref().unwrap_or_default() {
                    if let Some(ngen_msg) =
                        WhatsAppAdapter::webhook_message_to_ngenorca(msg, contact_name)
                    {
                        let event = Event {
                            id: EventId::new(),
                            timestamp: chrono::Utc::now(),
                            session_id: Some(ngen_msg.session_id.clone()),
                            user_id: ngen_msg.user_id.clone(),
                            payload: EventPayload::Message(ngen_msg),
                        };
                        if let Err(e) = sender.send(event) {
                            error!(error = %e, "Failed to send WhatsApp event to bus");
                        }
                    }
                }
            }
        }
    }

    /// Verify token getter (Cloud API mode only).
    pub fn verify_token(&self) -> &str {
        match &self.mode {
            WhatsAppMode::CloudApi { verify_token, .. } => verify_token,
            _ => "",
        }
    }

    /// Parse a raw Cloud API webhook body into NgenOrca Messages.
    ///
    /// Returns all successfully converted messages (empty vec on parse failure
    /// or if the payload contains no text messages).
    pub(crate) fn parse_webhook_messages(body: &[u8]) -> Vec<Message> {
        let payload: WaWebhookPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };

        let mut out = Vec::new();
        for entry in &payload.entry {
            for change in &entry.changes {
                if change.field != "messages" {
                    continue;
                }
                let contact_name = change
                    .value
                    .contacts
                    .as_ref()
                    .and_then(|c| c.first())
                    .and_then(|c| c.profile.as_ref())
                    .map(|p| p.name.as_str());

                for msg in change.value.messages.as_deref().unwrap_or_default() {
                    if let Some(m) = Self::webhook_message_to_ngenorca(msg, contact_name) {
                        out.push(m);
                    }
                }
            }
        }
        out
    }

    /// Webhook path getter (Cloud API mode only).
    pub fn webhook_path(&self) -> &str {
        match &self.mode {
            WhatsAppMode::CloudApi { webhook_path, .. } => webhook_path,
            _ => "",
        }
    }

    /// Whether this adapter is running in Native mode.
    pub fn is_native(&self) -> bool {
        matches!(self.mode, WhatsAppMode::Native { .. })
    }

    /// SEC-05: Verify a Cloud API webhook payload signature.
    ///
    /// WhatsApp signs payloads with HMAC-SHA256 using the app secret.
    /// The signature arrives in the `X-Hub-Signature-256` header as `sha256=<hex>`.
    pub fn verify_webhook_signature(&self, body: &[u8], signature_header: &str) -> bool {
        let secret = match &self.mode {
            WhatsAppMode::CloudApi {
                app_secret: Some(secret),
                ..
            } => secret,
            _ => return false, // No secret configured or not Cloud API mode
        };

        let expected_hex = match signature_header.strip_prefix("sha256=") {
            Some(hex) => hex,
            None => return false,
        };

        let Ok(expected_bytes) = hex_decode(expected_hex) else {
            return false;
        };

        let mut mac = <HmacSha256 as Mac>::new_from_slice(secret.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(body);
        mac.verify_slice(&expected_bytes).is_ok()
    }
}

#[async_trait]
impl Plugin for WhatsAppAdapter {
    fn manifest(&self) -> PluginManifest {
        let desc = if self.is_native() {
            "WhatsApp adapter — pure Rust native client (no external deps)"
        } else {
            "WhatsApp Cloud API adapter"
        };
        PluginManifest {
            id: PluginId("ngenorca-whatsapp".into()),
            name: "WhatsApp".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![
                Permission::ReadMessages,
                Permission::WriteMessages,
                Permission::Network,
            ],
            api_version: API_VERSION,
            description: desc.into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        match &self.mode {
            WhatsAppMode::Native { data_dir } => {
                info!(
                    data = %data_dir.display(),
                    "WhatsApp adapter initialized (Native mode — pure Rust)"
                );
            }
            WhatsAppMode::CloudApi {
                phone_number_id,
                webhook_path,
                access_token,
                ..
            } => {
                info!(
                    phone_number_id = %phone_number_id,
                    webhook_path = %webhook_path,
                    "WhatsApp adapter initialized (Cloud API, token len: {})",
                    access_token.len()
                );
            }
        }
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        if message.direction == Direction::Outbound {
            let to = &message.channel.0;
            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };

            match &self.mode {
                WhatsAppMode::Native { .. } => {
                    self.send_text_native(to, &text).await?;
                }
                WhatsAppMode::CloudApi {
                    phone_number_id,
                    access_token,
                    ..
                } => {
                    self.send_text_cloud(phone_number_id, access_token, to, &text)
                        .await?;
                }
            }
        }
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        match &self.mode {
            WhatsAppMode::Native { data_dir } => {
                // Native mode doesn't need external processes.
                // Check that data dir is writable.
                if !data_dir.exists() {
                    std::fs::create_dir_all(data_dir).map_err(|e| {
                        ngenorca_core::Error::Other(format!(
                            "WhatsApp: cannot create data dir {}: {e}",
                            data_dir.display()
                        ))
                    })?;
                }

                let guard = self.wa_client.lock().await;
                if let Some(ref wa) = *guard
                    && wa.is_connected()
                {
                    return Ok(());
                }
                // Not connected yet is OK during startup.
                Ok(())
            }
            WhatsAppMode::CloudApi {
                phone_number_id,
                access_token,
                ..
            } => {
                let url = format!("{GRAPH_API_BASE}/{phone_number_id}");
                let resp = self
                    .client
                    .get(&url)
                    .bearer_auth(access_token)
                    .send()
                    .await
                    .map_err(|e| ngenorca_core::Error::Other(format!("WhatsApp health: {e}")))?;

                if resp.status().is_success() {
                    Ok(())
                } else {
                    Err(ngenorca_core::Error::Other(
                        "WhatsApp health: token or phone_number_id invalid".into(),
                    ))
                }
            }
        }
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);

        if self.is_native() {
            let guard = self.wa_client.lock().await;
            if let Some(ref wa) = *guard {
                let _ = wa.disconnect().await;
            }
        }

        info!("WhatsApp adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for WhatsAppAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        self.running.store(true, Ordering::SeqCst);

        match &self.mode {
            WhatsAppMode::Native { data_dir } => {
                let sender = self.sender.clone().ok_or_else(|| {
                    ngenorca_core::Error::Other("WhatsApp: not initialized".into())
                })?;

                let running = self.running.clone();
                let data = data_dir.clone();
                let wa_client_handle = self.wa_client.clone();

                tokio::spawn(async move {
                    info!(
                        data_dir = %data.display(),
                        "WhatsApp: starting native client …"
                    );

                    let config = ngenorca_whatsapp_web::client::ClientConfig {
                        data_dir: data.clone(),
                        push_name: Some("NgenOrca".into()),
                    };

                    match WhatsAppClient::new(config).await {
                        Ok(mut client) => {
                            let mut event_rx = client.take_event_receiver().unwrap();

                            // Attempt connection.
                            match client.connect().await {
                                Ok(()) => {
                                    info!("WhatsApp: native client connected");
                                }
                                Err(e) => {
                                    error!(error = %e, "WhatsApp: native client connect failed");
                                    return;
                                }
                            }

                            // Store the client for send_text_native().
                            {
                                let mut guard = wa_client_handle.lock().await;
                                *guard = Some(client);
                            }

                            // Process incoming events.
                            while running.load(Ordering::SeqCst) {
                                match event_rx.recv().await {
                                    Some(WhatsAppEvent::QrCode(data)) => {
                                        info!(
                                            "WhatsApp: scan this QR code to link \
                                             your device:\n{data}"
                                        );
                                    }
                                    Some(WhatsAppEvent::Connected { jid }) => {
                                        info!(jid = %jid, "WhatsApp: connected");
                                    }
                                    Some(WhatsAppEvent::Message {
                                        from,
                                        text,
                                        timestamp,
                                        message_id,
                                    }) => {
                                        let ngen_msg = WhatsAppAdapter::native_event_to_ngenorca(
                                            &from,
                                            &text,
                                            timestamp,
                                            &message_id,
                                        );
                                        let event = Event {
                                            id: EventId::new(),
                                            timestamp: chrono::Utc::now(),
                                            session_id: Some(ngen_msg.session_id.clone()),
                                            user_id: ngen_msg.user_id.clone(),
                                            payload: EventPayload::Message(ngen_msg),
                                        };
                                        if let Err(e) = sender.send(event) {
                                            error!(
                                                error = %e,
                                                "WhatsApp: failed to send event"
                                            );
                                        }
                                    }
                                    Some(WhatsAppEvent::Disconnected { reason }) => {
                                        warn!(
                                            reason = %reason,
                                            "WhatsApp: disconnected"
                                        );
                                        if running.load(Ordering::SeqCst) {
                                            info!("WhatsApp: reconnecting in 5 s …");
                                            tokio::time::sleep(std::time::Duration::from_secs(5))
                                                .await;
                                            // Re-try connection.
                                            let mut guard = wa_client_handle.lock().await;
                                            if let Some(ref mut wa) = *guard
                                                && let Err(e) = wa.connect().await
                                            {
                                                error!(
                                                    error = %e,
                                                    "WhatsApp: reconnection failed"
                                                );
                                            }
                                        }
                                    }
                                    None => {
                                        info!("WhatsApp: event channel closed");
                                        break;
                                    }
                                }
                            }

                            // Clean up.
                            let mut guard = wa_client_handle.lock().await;
                            if let Some(ref wa) = *guard {
                                let _ = wa.disconnect().await;
                            }
                            *guard = None;
                        }
                        Err(e) => {
                            error!(error = %e, "WhatsApp: failed to create native client");
                        }
                    }
                });
            }
            WhatsAppMode::CloudApi { webhook_path, .. } => {
                // Cloud API mode — webhook-based.  The gateway's axum routes
                // forward POST payloads to `process_webhook_payload`.
                info!(
                    path = %webhook_path,
                    "WhatsApp adapter listening for webhook POSTs at {}",
                    webhook_path,
                );
            }
        }

        Ok(())
    }

    async fn send_message(&self, message: &Message) -> ngenorca_core::Result<()> {
        self.handle_message(message).await?;
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::WhatsApp
    }
}

// ─── WhatsApp Cloud API Types (legacy) ──────────────────────────

/// Top-level webhook payload from Meta.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct WaWebhookPayload {
    #[serde(default)]
    entry: Vec<WaEntry>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WaEntry {
    #[serde(default)]
    changes: Vec<WaChange>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WaChange {
    #[serde(default)]
    field: String,
    #[serde(default)]
    value: WaChangeValue,
}

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)]
struct WaChangeValue {
    #[serde(default)]
    messages: Option<Vec<WaMessage>>,
    #[serde(default)]
    contacts: Option<Vec<WaContact>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct WaMessage {
    #[serde(default)]
    id: String,
    from: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "type")]
    msg_type: Option<String>,
    #[serde(default)]
    text: Option<WaTextBody>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct WaTextBody {
    body: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WaContact {
    #[serde(default)]
    profile: Option<WaProfile>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct WaProfile {
    name: String,
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter_native() -> WhatsAppAdapter {
        WhatsAppAdapter::native(PathBuf::from("/tmp/wa-data"))
    }

    fn make_adapter_cloud() -> WhatsAppAdapter {
        WhatsAppAdapter::cloud_api(
            "123456789".into(),
            "EAAx-test-token".into(),
            "my_verify_secret".into(),
            "/webhook/whatsapp".into(),
            None,
        )
    }

    #[test]
    fn whatsapp_manifest_native() {
        let adapter = make_adapter_native();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "WhatsApp");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
        assert!(manifest.description.contains("native"));
    }

    #[test]
    fn whatsapp_manifest_cloud() {
        let adapter = make_adapter_cloud();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "WhatsApp");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
        assert!(manifest.description.contains("Cloud API"));
    }

    #[test]
    fn whatsapp_channel_kind() {
        let adapter = make_adapter_native();
        assert_eq!(adapter.channel_kind(), ChannelKind::WhatsApp);
    }

    #[test]
    fn whatsapp_is_native() {
        assert!(make_adapter_native().is_native());
        assert!(!make_adapter_cloud().is_native());
    }

    #[test]
    fn whatsapp_verify_token_cloud() {
        let adapter = make_adapter_cloud();
        assert_eq!(adapter.verify_token(), "my_verify_secret");
    }

    #[test]
    fn whatsapp_verify_token_native_empty() {
        let adapter = make_adapter_native();
        assert_eq!(adapter.verify_token(), "");
    }

    #[test]
    fn native_event_to_ngenorca_converts() {
        let ngen = WhatsAppAdapter::native_event_to_ngenorca(
            "5511999999999@s.whatsapp.net",
            "Hello from WhatsApp!",
            1700000000,
            "msg123",
        );
        assert_eq!(ngen.user_id, Some(UserId("whatsapp:5511999999999".into())));
        assert_eq!(ngen.channel_kind, ChannelKind::WhatsApp);
        assert_eq!(ngen.channel.0, "5511999999999");
        match &ngen.content {
            Content::Text(t) => assert_eq!(t, "Hello from WhatsApp!"),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn webhook_message_converts() {
        let msg = WaMessage {
            id: "wamid.123".into(),
            from: "15551234567".into(),
            timestamp: Some("1234567890".into()),
            msg_type: Some("text".into()),
            text: Some(WaTextBody {
                body: "Hello from WhatsApp!".into(),
            }),
        };
        let ngen = WhatsAppAdapter::webhook_message_to_ngenorca(&msg, Some("Test User")).unwrap();
        assert_eq!(ngen.user_id, Some(UserId("whatsapp:15551234567".into())));
        assert_eq!(ngen.channel_kind, ChannelKind::WhatsApp);
        match &ngen.content {
            Content::Text(t) => assert_eq!(t, "Hello from WhatsApp!"),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn webhook_message_no_text_returns_none() {
        let msg = WaMessage {
            id: "wamid.456".into(),
            from: "15551234567".into(),
            timestamp: None,
            msg_type: Some("image".into()),
            text: None,
        };
        assert!(WhatsAppAdapter::webhook_message_to_ngenorca(&msg, None).is_none());
    }

    #[test]
    fn process_webhook_payload_sends_events() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = flume_like::Sender::new(tx);

        let payload = WaWebhookPayload {
            entry: vec![WaEntry {
                changes: vec![WaChange {
                    field: "messages".into(),
                    value: WaChangeValue {
                        messages: Some(vec![WaMessage {
                            id: "wamid.789".into(),
                            from: "15559998888".into(),
                            timestamp: None,
                            msg_type: Some("text".into()),
                            text: Some(WaTextBody {
                                body: "Hey NgenOrca".into(),
                            }),
                        }]),
                        contacts: None,
                    },
                }],
            }],
        };

        WhatsAppAdapter::process_webhook_payload(&payload, &sender);
        let event = rx.try_recv().expect("should have received an event");
        match event.payload {
            EventPayload::Message(msg) => {
                assert_eq!(msg.channel_kind, ChannelKind::WhatsApp);
                match &msg.content {
                    Content::Text(t) => assert_eq!(t, "Hey NgenOrca"),
                    _ => panic!("Expected text"),
                }
            }
            _ => panic!("Expected message event"),
        }
    }
}
