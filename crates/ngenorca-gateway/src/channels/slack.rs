//! Slack Bot adapter — Socket Mode (WebSocket) or Events API.
//!
//! Connects to Slack using either Socket Mode (recommended for homelab — no
//! public URL needed) or the Events API webhook.  Converts Slack events into
//! NgenOrca `Message` types and pushes them to the event bus.
//!
//! ## Socket Mode flow
//! 1. POST `apps.connections.open` with app-level token → receive WSS URL.
//! 2. Connect to WSS, receive `hello`, then `events_api` envelopes.
//! 3. Acknowledge each envelope with `{ "envelope_id": "..." }`.
//! 4. Extract `message` events, convert, push to bus.
//!
//! ## Send path
//! POST `chat.postMessage` with bot token.

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use ngenorca_core::ChannelKind;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{API_VERSION, Permission, PluginKind, PluginManifest};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext, flume_like};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, error, info, warn};

use hmac::{Hmac, Mac};
use sha2::Sha256;
type HmacSha256 = Hmac<Sha256>;

const SLACK_API: &str = "https://slack.com/api";

/// Slack Bot adapter.
pub struct SlackAdapter {
    bot_token: String,
    app_token: Option<String>,
    /// Signing secret for verifying incoming webhook requests (Events API).
    #[allow(dead_code)]
    pub(crate) signing_secret: Option<String>,
    socket_mode: bool,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    client: reqwest::Client,
}

impl SlackAdapter {
    pub fn new(
        bot_token: String,
        app_token: Option<String>,
        socket_mode: bool,
        signing_secret: Option<String>,
    ) -> Self {
        Self {
            bot_token,
            app_token,
            signing_secret,
            socket_mode,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::new(),
        }
    }

    /// SEC-05: Verify a Slack Events API webhook request.
    ///
    /// Slack sends `X-Slack-Signature` = `v0=<HMAC-SHA256(signing_secret, "v0:{ts}:{body}")>`
    /// and `X-Slack-Request-Timestamp` headers.
    ///
    /// Returns `true` if the signature is valid.
    #[allow(dead_code)]
    pub fn verify_webhook_signature(
        signing_secret: &str,
        timestamp: &str,
        body: &[u8],
        signature_header: &str,
    ) -> bool {
        let expected_hex = match signature_header.strip_prefix("v0=") {
            Some(hex) => hex,
            None => return false,
        };

        // Reject timestamps older than 5 minutes to prevent replay attacks.
        if let Ok(ts) = timestamp.parse::<i64>() {
            let now = chrono::Utc::now().timestamp();
            if (now - ts).abs() > 300 {
                return false;
            }
        }

        let basestring = format!("v0:{timestamp}:{}", String::from_utf8_lossy(body));
        let mut mac = <HmacSha256 as Mac>::new_from_slice(signing_secret.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(basestring.as_bytes());
        let result = mac.finalize().into_bytes();
        let computed: String = result.iter().map(|b| format!("{b:02x}")).collect();

        // Constant-time comparison via the hmac crate's internals + string equality
        // length check. For defense-in-depth, use subtle for the hex string compare.
        use subtle::ConstantTimeEq;
        let a = computed.as_bytes();
        let b = expected_hex.as_bytes();
        a.len() == b.len() && a.ct_eq(b).into()
    }

    /// Open a Socket Mode connection and return the WSS URL.
    async fn open_socket_mode_connection(&self) -> ngenorca_core::Result<String> {
        let app_token = self.app_token.as_deref().ok_or_else(|| {
            ngenorca_core::Error::Gateway("Slack Socket Mode requires an app_token".into())
        })?;

        let resp = self
            .client
            .post(format!("{SLACK_API}/apps.connections.open"))
            .bearer_auth(app_token)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Slack connections.open: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Slack parse error: {e}")))?;

        if body["ok"].as_bool() != Some(true) {
            return Err(ngenorca_core::Error::Gateway(format!(
                "Slack connections.open failed: {}",
                body["error"].as_str().unwrap_or("unknown")
            )));
        }

        body["url"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| ngenorca_core::Error::Gateway("No WSS URL in Slack response".into()))
    }

    /// Convert a Slack message event into an NgenOrca Message.
    fn slack_event_to_message(event: &SlackMessageEvent, channel_id: &str) -> Option<Message> {
        // Skip bot messages to avoid echoes.
        if event.bot_id.is_some() || event.subtype.is_some() {
            return None;
        }

        let text = event.text.as_deref().unwrap_or("");
        if text.is_empty() {
            return None;
        }

        Some(Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: event.user.as_ref().map(|u| UserId(format!("slack:{u}"))),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId(channel_id.to_string()),
            channel_kind: ChannelKind::Slack,
            direction: Direction::Inbound,
            content: Content::Text(text.to_string()),
            metadata: serde_json::json!({
                "slack_user": event.user,
                "slack_channel": channel_id,
                "slack_ts": event.ts,
            }),
        })
    }

    /// Send a text to a Slack channel.
    async fn post_message(&self, channel: &str, text: &str) -> ngenorca_core::Result<()> {
        let body = serde_json::json!({
            "channel": channel,
            "text": text,
        });

        let resp = self
            .client
            .post(format!("{SLACK_API}/chat.postMessage"))
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Slack postMessage: {e}")))?;

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Slack parse: {e}")))?;

        if result["ok"].as_bool() != Some(true) {
            return Err(ngenorca_core::Error::Other(format!(
                "Slack chat.postMessage error: {}",
                result["error"].as_str().unwrap_or("unknown")
            )));
        }

        Ok(())
    }
}

#[async_trait]
impl Plugin for SlackAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-slack".into()),
            name: "Slack".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![
                Permission::ReadMessages,
                Permission::WriteMessages,
                Permission::Network,
            ],
            api_version: API_VERSION,
            description: "Slack Bot adapter (Socket Mode or Webhook)".into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        info!(
            socket_mode = self.socket_mode,
            has_app_token = self.app_token.is_some(),
            "Slack adapter initialized (bot token length: {})",
            self.bot_token.len()
        );
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        if message.direction == Direction::Outbound {
            let channel = &message.channel.0;
            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };
            self.post_message(channel, &text).await?;
        }
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        let resp = self
            .client
            .post(format!("{SLACK_API}/auth.test"))
            .bearer_auth(&self.bot_token)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Slack health check: {e}")))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Slack parse: {e}")))?;

        if body["ok"].as_bool() == Some(true) {
            Ok(())
        } else {
            Err(ngenorca_core::Error::Other(format!(
                "Slack auth.test failed: {}",
                body["error"].as_str().unwrap_or("unknown")
            )))
        }
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        info!("Slack adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for SlackAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        if !self.socket_mode {
            info!("Slack adapter in Events API mode — waiting for webhook pushes");
            return Ok(());
        }

        let wss_url = self.open_socket_mode_connection().await?;
        self.running.store(true, Ordering::SeqCst);

        let running = self.running.clone();
        let sender = self
            .sender
            .clone()
            .ok_or_else(|| ngenorca_core::Error::Other("Sender not initialized".into()))?;
        let app_token = self.app_token.clone().unwrap_or_default();
        let client = self.client.clone();

        tokio::spawn(async move {
            info!("Slack Socket Mode connecting…");

            // Outer reconnection loop.
            let mut current_url = wss_url;
            while running.load(Ordering::SeqCst) {
                match tokio_tungstenite::connect_async(&current_url).await {
                    Ok((ws_stream, _)) => {
                        info!("Slack Socket Mode connected");
                        let (mut write, mut read) = ws_stream.split();

                        while running.load(Ordering::SeqCst) {
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(45),
                                read.next(),
                            )
                            .await
                            {
                                Ok(Some(Ok(msg))) => {
                                    let text = match msg {
                                        tokio_tungstenite::tungstenite::Message::Text(t) => {
                                            t.to_string()
                                        }
                                        tokio_tungstenite::tungstenite::Message::Ping(d) => {
                                            let _ = write
                                                .send(
                                                    tokio_tungstenite::tungstenite::Message::Pong(
                                                        d,
                                                    ),
                                                )
                                                .await;
                                            continue;
                                        }
                                        _ => continue,
                                    };

                                    // Parse the Socket Mode envelope.
                                    let envelope: serde_json::Value =
                                        match serde_json::from_str(&text) {
                                            Ok(v) => v,
                                            Err(_) => continue,
                                        };

                                    // Acknowledge the envelope.
                                    if let Some(eid) = envelope["envelope_id"].as_str() {
                                        let ack =
                                            serde_json::json!({"envelope_id": eid}).to_string();
                                        let _ = write
                                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                                ack.into(),
                                            ))
                                            .await;
                                    }

                                    // Process events_api envelopes.
                                    if envelope["type"].as_str() == Some("events_api")
                                        && let Some(inner) =
                                            envelope["payload"]["event"].as_object()
                                        && inner.get("type").and_then(|v| v.as_str())
                                            == Some("message")
                                    {
                                        let evt: std::result::Result<SlackMessageEvent, _> =
                                            serde_json::from_value(serde_json::Value::Object(
                                                inner.clone(),
                                            ));
                                        if let Ok(slack_evt) = evt {
                                            let ch = inner
                                                .get("channel")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown");
                                            if let Some(ngen_msg) =
                                                SlackAdapter::slack_event_to_message(&slack_evt, ch)
                                            {
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
                                                        "Failed to send Slack event to bus"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    // Handle disconnect advisory.
                                    if envelope["type"].as_str() == Some("disconnect") {
                                        warn!("Slack sent disconnect advisory — reconnecting");
                                        break;
                                    }
                                }
                                Ok(Some(Err(e))) => {
                                    warn!(error = %e, "Slack WS error");
                                    break;
                                }
                                Ok(None) => {
                                    warn!("Slack WS stream ended");
                                    break;
                                }
                                Err(_) => {
                                    debug!("Slack WS read timeout — keepalive ok");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Slack Socket Mode connection failed");
                    }
                }

                if running.load(Ordering::SeqCst) {
                    info!("Slack Socket Mode reconnecting in 5s…");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    // Get a new WSS URL for reconnection.
                    let resp = client
                        .post(format!("{SLACK_API}/apps.connections.open"))
                        .bearer_auth(&app_token)
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .send()
                        .await;
                    if let Ok(r) = resp
                        && let Ok(body) = r.json::<serde_json::Value>().await
                        && let Some(url) = body["url"].as_str()
                    {
                        current_url = url.to_string();
                    }
                }
            }

            info!("Slack Socket Mode stopped");
        });

        Ok(())
    }

    async fn send_message(&self, message: &Message) -> ngenorca_core::Result<()> {
        self.handle_message(message).await?;
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Slack
    }
}

// ─── Slack API Deserialization Types ─────────────────────────────

#[derive(Debug, Deserialize)]
struct SlackMessageEvent {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter() -> SlackAdapter {
        SlackAdapter::new(
            "xoxb-test-token".into(),
            Some("xapp-test".into()),
            true,
            None,
        )
    }

    #[test]
    fn slack_manifest() {
        let adapter = make_adapter();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Slack");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[test]
    fn slack_channel_kind() {
        let adapter = make_adapter();
        assert_eq!(adapter.channel_kind(), ChannelKind::Slack);
    }

    #[test]
    fn slack_event_to_message_converts() {
        let evt = SlackMessageEvent {
            user: Some("U1234".into()),
            text: Some("Hello from Slack!".into()),
            ts: Some("1234567890.123456".into()),
            bot_id: None,
            subtype: None,
        };
        let msg = SlackAdapter::slack_event_to_message(&evt, "C5678").unwrap();
        assert_eq!(msg.user_id, Some(UserId("slack:U1234".into())));
        assert_eq!(msg.channel_kind, ChannelKind::Slack);
        assert_eq!(msg.channel, ChannelId("C5678".into()));
        match &msg.content {
            Content::Text(t) => assert_eq!(t, "Hello from Slack!"),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn slack_event_skips_bot_messages() {
        let evt = SlackMessageEvent {
            user: Some("U1234".into()),
            text: Some("bot reply".into()),
            ts: Some("123".into()),
            bot_id: Some("B9999".into()),
            subtype: None,
        };
        assert!(SlackAdapter::slack_event_to_message(&evt, "C1").is_none());
    }

    #[test]
    fn slack_event_skips_empty_text() {
        let evt = SlackMessageEvent {
            user: Some("U1234".into()),
            text: None,
            ts: None,
            bot_id: None,
            subtype: None,
        };
        assert!(SlackAdapter::slack_event_to_message(&evt, "C1").is_none());
    }
}
