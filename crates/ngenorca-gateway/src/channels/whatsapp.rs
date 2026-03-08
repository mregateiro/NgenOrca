//! WhatsApp Cloud API adapter.
//!
//! Uses Meta's WhatsApp Business Cloud API for sending and receiving messages.
//!
//! ## Receive path (webhook)
//! WhatsApp sends POST requests to a webhook endpoint.  The gateway registers
//! the webhook at the configured path.  Each incoming notification contains a
//! `messages` array with the inbound messages.
//!
//! The adapter also handles the GET verification challenge that WhatsApp sends
//! when you first register the webhook URL.
//!
//! ## Send path
//! `POST https://graph.facebook.com/v21.0/{phone_number_id}/messages`
//! with a bearer access token.

use async_trait::async_trait;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{flume_like, ChannelAdapter, Plugin, PluginContext};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info};

const GRAPH_API_BASE: &str = "https://graph.facebook.com/v21.0";

/// WhatsApp Cloud API adapter.
pub struct WhatsAppAdapter {
    phone_number_id: String,
    access_token: String,
    verify_token: String,
    webhook_path: String,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    client: reqwest::Client,
}

impl WhatsAppAdapter {
    pub fn new(
        phone_number_id: String,
        access_token: String,
        verify_token: String,
        webhook_path: String,
    ) -> Self {
        Self {
            phone_number_id,
            access_token,
            verify_token,
            webhook_path,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::new(),
        }
    }

    /// Send a text message to a WhatsApp number.
    async fn send_text(&self, to: &str, text: &str) -> ngenorca_core::Result<()> {
        let url = format!("{GRAPH_API_BASE}/{}/messages", self.phone_number_id);

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
            .bearer_auth(&self.access_token)
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

        debug!(to = to, "WhatsApp message sent");
        Ok(())
    }

    /// Convert a WhatsApp webhook message into an NgenOrca Message.
    #[allow(dead_code)]
    pub(crate) fn webhook_message_to_ngenorca(msg: &WaMessage, contact_name: Option<&str>) -> Option<Message> {
        // Only handle text messages for now.
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

    /// Process a webhook notification payload.
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

                // Build a quick contact-name lookup.
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

    /// Verify token getter (used by the webhook verification endpoint).
    pub fn verify_token(&self) -> &str {
        &self.verify_token
    }

    /// Webhook path getter.
    pub fn webhook_path(&self) -> &str {
        &self.webhook_path
    }
}

#[async_trait]
impl Plugin for WhatsAppAdapter {
    fn manifest(&self) -> PluginManifest {
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
            description: "WhatsApp Cloud API adapter".into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        info!(
            phone_number_id = %self.phone_number_id,
            webhook_path = %self.webhook_path,
            "WhatsApp adapter initialized (token length: {})",
            self.access_token.len()
        );
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        if message.direction == Direction::Outbound {
            let to = &message.channel.0;
            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };
            self.send_text(to, &text).await?;
        }
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // Verify token by fetching the WhatsApp Business phone number profile.
        let url = format!("{GRAPH_API_BASE}/{}", self.phone_number_id);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("WhatsApp health: {e}")))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ngenorca_core::Error::Other(
                "WhatsApp health check: token or phone_number_id invalid".into(),
            ))
        }
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        info!("WhatsApp adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for WhatsAppAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // WhatsApp is webhook-based — Meta POSTs to our endpoint.
        // The actual axum route for the webhook would be registered by the
        // gateway server.  Here we just validate config and log readiness.
        //
        // In a full setup, the gateway's `server.rs` would add:
        //   .route(&webhook_path, get(wa_verify).post(wa_webhook))
        // and forward the parsed payload to `process_webhook_payload`.

        self.running.store(true, Ordering::SeqCst);

        info!(
            path = %self.webhook_path,
            phone = %self.phone_number_id,
            "WhatsApp adapter listening for webhook POSTs at {}",
            self.webhook_path,
        );

        // Spawn a lightweight task that processes webhook payloads pushed via
        // an internal channel.  For now the gateway can call
        // `process_webhook_payload` directly from the webhook route handler.

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

// ─── WhatsApp Cloud API Types ────────────────────────────────────

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

    fn make_adapter() -> WhatsAppAdapter {
        WhatsAppAdapter::new(
            "123456789".into(),
            "EAAx-test-token".into(),
            "my_verify_secret".into(),
            "/webhook/whatsapp".into(),
        )
    }

    #[test]
    fn whatsapp_manifest() {
        let adapter = make_adapter();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "WhatsApp");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[test]
    fn whatsapp_channel_kind() {
        let adapter = make_adapter();
        assert_eq!(adapter.channel_kind(), ChannelKind::WhatsApp);
    }

    #[test]
    fn whatsapp_verify_token() {
        let adapter = make_adapter();
        assert_eq!(adapter.verify_token(), "my_verify_secret");
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

