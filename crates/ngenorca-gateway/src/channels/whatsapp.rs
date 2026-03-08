//! WhatsApp adapter — dual-mode: Baileys bridge or Cloud API.
//!
//! ## Mode 1 — Baileys bridge (preferred, no public URL needed)
//! Spawns a Node.js child process running the Baileys WhatsApp Web bridge.
//! Messages arrive on stdout as newline-delimited JSON; outgoing messages
//! are written to stdin.  Authentication happens via QR-code scan (first
//! run) with session data persisted to `data_dir`.
//!
//! ## Mode 2 — Cloud API (legacy, requires public webhook URL)
//! Uses Meta's WhatsApp Business Cloud API for sending and receiving
//! messages.  Requires `access_token` and `phone_number_id` in config.
//!
//! The adapter auto-detects the mode based on config:
//! - If `bridge_path` is set → Baileys mode
//! - Otherwise falls back to Cloud API mode

use async_trait::async_trait;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{flume_like, ChannelAdapter, Plugin, PluginContext};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

const GRAPH_API_BASE: &str = "https://graph.facebook.com/v21.0";

/// Operational mode for the WhatsApp adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhatsAppMode {
    /// Baileys bridge — Node.js child process using WhatsApp Web protocol.
    Baileys {
        bridge_path: PathBuf,
        data_dir: PathBuf,
    },
    /// Cloud API — Meta's official REST + webhook approach.
    CloudApi {
        phone_number_id: String,
        access_token: String,
        verify_token: String,
        webhook_path: String,
    },
}

/// WhatsApp adapter supporting Baileys bridge and Cloud API modes.
pub struct WhatsAppAdapter {
    mode: WhatsAppMode,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    client: reqwest::Client,
    /// Handle to the Baileys bridge's stdin for sending messages.
    bridge_stdin: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
}

impl WhatsAppAdapter {
    /// Create a new WhatsApp adapter in the given mode.
    pub fn new(mode: WhatsAppMode) -> Self {
        Self {
            mode,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::new(),
            bridge_stdin: Arc::new(Mutex::new(None)),
        }
    }

    /// Convenience constructor for Baileys mode.
    pub fn baileys(bridge_path: PathBuf, data_dir: PathBuf) -> Self {
        Self::new(WhatsAppMode::Baileys {
            bridge_path,
            data_dir,
        })
    }

    /// Convenience constructor for Cloud API mode (backward-compatible).
    pub fn cloud_api(
        phone_number_id: String,
        access_token: String,
        verify_token: String,
        webhook_path: String,
    ) -> Self {
        Self::new(WhatsAppMode::CloudApi {
            phone_number_id,
            access_token,
            verify_token,
            webhook_path,
        })
    }

    // ── Baileys bridge helpers ──────────────────────────────────

    /// Send a text message via the Baileys bridge's stdin.
    async fn send_text_baileys(&self, to: &str, text: &str) -> ngenorca_core::Result<()> {
        let mut guard = self.bridge_stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| {
            ngenorca_core::Error::Other("WhatsApp Baileys bridge stdin not available".into())
        })?;

        let cmd = serde_json::json!({
            "action": "send",
            "to": to,
            "text": text,
        });

        let mut line = serde_json::to_string(&cmd)
            .map_err(|e| ngenorca_core::Error::Other(format!("WhatsApp bridge serialize: {e}")))?;
        line.push('\n');

        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("WhatsApp bridge write: {e}")))?;

        debug!(to = to, "WhatsApp message sent via Baileys bridge");
        Ok(())
    }

    /// Convert a Baileys bridge JSON message into an NgenOrca `Message`.
    pub(crate) fn bridge_message_to_ngenorca(msg: &BaileysMessage) -> Option<Message> {
        let text = msg.text.as_deref()?;
        if text.is_empty() {
            return None;
        }

        // Baileys JIDs look like `5511999999999@s.whatsapp.net` for individual
        // or `120363XXXXX@g.us` for groups.  Strip the suffix for user id.
        let sender = msg.sender.as_deref().unwrap_or(&msg.from);
        let phone = sender.split('@').next().unwrap_or(sender);

        // Use chat JID as channel (group or individual).
        let channel = msg.from.split('@').next().unwrap_or(&msg.from);

        Some(Message {
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
                "whatsapp_from": msg.from,
                "whatsapp_sender": msg.sender,
                "whatsapp_message_id": msg.id,
                "whatsapp_push_name": msg.push_name,
                "whatsapp_is_group": msg.is_group,
            }),
        })
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
    pub(crate) fn process_webhook_payload(
        payload: &WaWebhookPayload,
        sender: &flume_like::Sender,
    ) {
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

    /// Webhook path getter (Cloud API mode only).
    pub fn webhook_path(&self) -> &str {
        match &self.mode {
            WhatsAppMode::CloudApi { webhook_path, .. } => webhook_path,
            _ => "",
        }
    }

    /// Whether this adapter is running in Baileys mode.
    pub fn is_baileys(&self) -> bool {
        matches!(self.mode, WhatsAppMode::Baileys { .. })
    }
}

#[async_trait]
impl Plugin for WhatsAppAdapter {
    fn manifest(&self) -> PluginManifest {
        let desc = if self.is_baileys() {
            "WhatsApp adapter via Baileys bridge (WhatsApp Web protocol)"
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
            WhatsAppMode::Baileys {
                bridge_path,
                data_dir,
            } => {
                info!(
                    bridge = %bridge_path.display(),
                    data = %data_dir.display(),
                    "WhatsApp adapter initialized (Baileys mode)"
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
                WhatsAppMode::Baileys { .. } => {
                    self.send_text_baileys(to, &text).await?;
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
            WhatsAppMode::Baileys { bridge_path, .. } => {
                // Verify that Node.js and the bridge script exist.
                let output = Command::new("node")
                    .arg("--version")
                    .output()
                    .await
                    .map_err(|e| {
                        ngenorca_core::Error::Other(format!(
                            "WhatsApp Baileys health: node not found: {e}"
                        ))
                    })?;

                if !output.status.success() {
                    return Err(ngenorca_core::Error::Other(
                        "WhatsApp Baileys: node --version failed".into(),
                    ));
                }

                if !bridge_path.exists() {
                    return Err(ngenorca_core::Error::Other(format!(
                        "WhatsApp Baileys: bridge script not found at {}",
                        bridge_path.display()
                    )));
                }

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
                    .map_err(|e| {
                        ngenorca_core::Error::Other(format!("WhatsApp health: {e}"))
                    })?;

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

        if self.is_baileys() {
            // Drop the bridge's stdin so the child process exits.
            let mut stdin = self.bridge_stdin.lock().await;
            *stdin = None;
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
            WhatsAppMode::Baileys {
                bridge_path,
                data_dir,
            } => {
                let sender = self
                    .sender
                    .clone()
                    .ok_or_else(|| {
                        ngenorca_core::Error::Other("WhatsApp: not initialized".into())
                    })?;

                let running = self.running.clone();
                let bridge = bridge_path.clone();
                let data = data_dir.clone();
                let bridge_stdin = self.bridge_stdin.clone();

                tokio::spawn(async move {
                    while running.load(Ordering::SeqCst) {
                        info!(
                            bridge = %bridge.display(),
                            data_dir = %data.display(),
                            "WhatsApp: spawning Baileys bridge …"
                        );

                        let child = Command::new("node")
                            .arg(&bridge)
                            .arg("--data-dir")
                            .arg(&data)
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn();

                        match child {
                            Ok(mut child) => {
                                // Store stdin handle for sending.
                                if let Some(stdin) = child.stdin.take() {
                                    let mut guard = bridge_stdin.lock().await;
                                    *guard = Some(stdin);
                                }

                                // Read newline-delimited JSON from stdout.
                                if let Some(stdout) = child.stdout.take() {
                                    let reader = BufReader::new(stdout);
                                    let mut lines = reader.lines();

                                    loop {
                                        if !running.load(Ordering::SeqCst) {
                                            break;
                                        }

                                        match lines.next_line().await {
                                            Ok(Some(line)) => {
                                                if line.trim().is_empty() {
                                                    continue;
                                                }

                                                // Handle QR code output for initial pairing.
                                                if let Ok(evt) =
                                                    serde_json::from_str::<BaileysEvent>(&line)
                                                {
                                                    match evt.event.as_str() {
                                                        "qr" => {
                                                            info!(
                                                                "WhatsApp: scan this QR code \
                                                                 to link your device:\n{}",
                                                                evt.data
                                                                    .as_deref()
                                                                    .unwrap_or("(no data)")
                                                            );
                                                            continue;
                                                        }
                                                        "connected" => {
                                                            info!(
                                                                "WhatsApp: Baileys connected \
                                                                 successfully"
                                                            );
                                                            continue;
                                                        }
                                                        "message" => {
                                                            // Fall through to message parsing.
                                                        }
                                                        other => {
                                                            debug!(
                                                                event = other,
                                                                "WhatsApp bridge event"
                                                            );
                                                            continue;
                                                        }
                                                    }
                                                }

                                                // Parse as incoming message.
                                                match serde_json::from_str::<BaileysOutput>(
                                                    &line,
                                                ) {
                                                    Ok(output) => {
                                                        if let Some(ref msg) = output.message
                                                            && let Some(ngen_msg) =
                                                                WhatsAppAdapter::bridge_message_to_ngenorca(msg)
                                                        {
                                                            let event = Event {
                                                                id: EventId::new(),
                                                                timestamp: chrono::Utc::now(),
                                                                session_id: Some(
                                                                    ngen_msg.session_id.clone(),
                                                                ),
                                                                user_id: ngen_msg.user_id.clone(),
                                                                payload: EventPayload::Message(
                                                                    ngen_msg,
                                                                ),
                                                            };
                                                            if let Err(e) = sender.send(event) {
                                                                error!(
                                                                    error = %e,
                                                                    "WhatsApp: failed to send \
                                                                     event"
                                                                );
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        debug!(
                                                            error = %e,
                                                            line = %line,
                                                            "WhatsApp: failed to parse bridge \
                                                             output"
                                                        );
                                                    }
                                                }
                                            }
                                            Ok(None) => {
                                                info!("WhatsApp: bridge stdout closed");
                                                break;
                                            }
                                            Err(e) => {
                                                error!(
                                                    error = %e,
                                                    "WhatsApp: bridge read error"
                                                );
                                                break;
                                            }
                                        }
                                    }
                                }

                                // Clean up stdin handle.
                                {
                                    let mut guard = bridge_stdin.lock().await;
                                    *guard = None;
                                }

                                match child.wait().await {
                                    Ok(status) => {
                                        info!(
                                            status = %status,
                                            "WhatsApp: Baileys bridge exited"
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            error = %e,
                                            "WhatsApp: bridge wait error"
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                error!(
                                    error = %e,
                                    "WhatsApp: failed to spawn Baileys bridge"
                                );
                            }
                        }

                        if running.load(Ordering::SeqCst) {
                            info!("WhatsApp: restarting Baileys bridge in 5 s …");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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

// ─── Baileys Bridge JSON Types ──────────────────────────────────

/// Bridge event envelope (for QR codes, connection status, etc.)
#[derive(Debug, Deserialize)]
pub(crate) struct BaileysEvent {
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub data: Option<String>,
}

/// Top-level JSON output from the Baileys bridge for messages.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct BaileysOutput {
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub message: Option<BaileysMessage>,
}

/// A single WhatsApp message from the Baileys bridge.
#[derive(Debug, Deserialize)]
pub(crate) struct BaileysMessage {
    /// Message ID from WhatsApp.
    #[serde(default)]
    pub id: Option<String>,
    /// Chat JID (e.g. `5511999999999@s.whatsapp.net` or `120363XXX@g.us`).
    pub from: String,
    /// Sender JID (differs from `from` in group chats).
    #[serde(default)]
    pub sender: Option<String>,
    /// Display name of the sender.
    #[serde(default)]
    pub push_name: Option<String>,
    /// Text body (only present for text messages).
    #[serde(default)]
    pub text: Option<String>,
    /// Whether this message is from a group chat.
    #[serde(default)]
    pub is_group: bool,
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

    fn make_adapter_baileys() -> WhatsAppAdapter {
        WhatsAppAdapter::baileys(
            PathBuf::from("/opt/ngenorca/whatsapp-bridge.js"),
            PathBuf::from("/tmp/wa-data"),
        )
    }

    fn make_adapter_cloud() -> WhatsAppAdapter {
        WhatsAppAdapter::cloud_api(
            "123456789".into(),
            "EAAx-test-token".into(),
            "my_verify_secret".into(),
            "/webhook/whatsapp".into(),
        )
    }

    #[test]
    fn whatsapp_manifest_baileys() {
        let adapter = make_adapter_baileys();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "WhatsApp");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
        assert!(manifest.description.contains("Baileys"));
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
        let adapter = make_adapter_baileys();
        assert_eq!(adapter.channel_kind(), ChannelKind::WhatsApp);
    }

    #[test]
    fn whatsapp_is_baileys() {
        assert!(make_adapter_baileys().is_baileys());
        assert!(!make_adapter_cloud().is_baileys());
    }

    #[test]
    fn whatsapp_verify_token_cloud() {
        let adapter = make_adapter_cloud();
        assert_eq!(adapter.verify_token(), "my_verify_secret");
    }

    #[test]
    fn whatsapp_verify_token_baileys_empty() {
        let adapter = make_adapter_baileys();
        assert_eq!(adapter.verify_token(), "");
    }

    #[test]
    fn bridge_message_converts() {
        let msg = BaileysMessage {
            id: Some("3EB0A1B2C3D4".into()),
            from: "5511999999999@s.whatsapp.net".into(),
            sender: None,
            push_name: Some("Alice".into()),
            text: Some("Hello from WhatsApp!".into()),
            is_group: false,
        };
        let ngen = WhatsAppAdapter::bridge_message_to_ngenorca(&msg).unwrap();
        assert_eq!(
            ngen.user_id,
            Some(UserId("whatsapp:5511999999999".into()))
        );
        assert_eq!(ngen.channel_kind, ChannelKind::WhatsApp);
        assert_eq!(ngen.channel.0, "5511999999999");
        match &ngen.content {
            Content::Text(t) => assert_eq!(t, "Hello from WhatsApp!"),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn bridge_group_message_uses_sender() {
        let msg = BaileysMessage {
            id: Some("msg123".into()),
            from: "120363001234@g.us".into(),
            sender: Some("5511888888888@s.whatsapp.net".into()),
            push_name: Some("Bob".into()),
            text: Some("Group msg".into()),
            is_group: true,
        };
        let ngen = WhatsAppAdapter::bridge_message_to_ngenorca(&msg).unwrap();
        // user_id should be the actual sender, not the group
        assert_eq!(
            ngen.user_id,
            Some(UserId("whatsapp:5511888888888".into()))
        );
        // channel should be the group
        assert_eq!(ngen.channel.0, "120363001234");
    }

    #[test]
    fn bridge_empty_text_skipped() {
        let msg = BaileysMessage {
            id: None,
            from: "5511999999999@s.whatsapp.net".into(),
            sender: None,
            push_name: None,
            text: Some("".into()),
            is_group: false,
        };
        assert!(WhatsAppAdapter::bridge_message_to_ngenorca(&msg).is_none());
    }

    #[test]
    fn bridge_no_text_skipped() {
        let msg = BaileysMessage {
            id: None,
            from: "5511999999999@s.whatsapp.net".into(),
            sender: None,
            push_name: None,
            text: None,
            is_group: false,
        };
        assert!(WhatsAppAdapter::bridge_message_to_ngenorca(&msg).is_none());
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
        let ngen =
            WhatsAppAdapter::webhook_message_to_ngenorca(&msg, Some("Test User")).unwrap();
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

