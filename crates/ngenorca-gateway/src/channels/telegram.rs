//! Telegram Bot adapter — long-polling (or webhook) integration.
//!
//! Uses the Telegram Bot API to receive and send messages.
//! Supports:
//! - Long-polling (`getUpdates`) — default, works behind NAT/VPN
//! - Webhook mode — requires a public URL
//!
//! The adapter converts between Telegram's message format and NgenOrca's
//! internal `Message` type, handling user identification via Telegram user IDs.

use async_trait::async_trait;
use ngenorca_core::ChannelKind;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{API_VERSION, Permission, PluginKind, PluginManifest};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext, flume_like};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tracing::{debug, error, info, warn};

use subtle::ConstantTimeEq;

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
const POLL_TIMEOUT_SECS: u64 = 30;

/// Telegram Bot adapter.
pub struct TelegramAdapter {
    /// Bot token from @BotFather.
    token: String,
    /// Use long-polling mode.
    polling: bool,
    /// Optional webhook URL.
    webhook_url: Option<String>,
    /// Allowed Telegram user IDs (empty = allow all).
    allowed_users: Vec<i64>,
    /// Event sender for pushing inbound messages to the bus.
    sender: Option<flume_like::Sender>,
    /// Whether the adapter is running.
    running: Arc<AtomicBool>,
    /// Last processed update ID (for polling offset).
    last_update_id: Arc<AtomicI64>,
    /// HTTP client.
    client: reqwest::Client,
}

impl TelegramAdapter {
    pub fn new(
        token: String,
        polling: bool,
        webhook_url: Option<String>,
        allowed_users: Vec<i64>,
    ) -> Self {
        Self {
            token,
            polling,
            webhook_url,
            allowed_users,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            last_update_id: Arc::new(AtomicI64::new(0)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(POLL_TIMEOUT_SECS + 10))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Build Telegram API URL for a method.
    fn api_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", TELEGRAM_API_BASE, self.token, method)
    }

    /// Check if a user is allowed.
    #[allow(dead_code)]
    fn is_user_allowed(&self, user_id: i64) -> bool {
        self.allowed_users.is_empty() || self.allowed_users.contains(&user_id)
    }

    /// SEC-05: Verify a Telegram webhook request.
    ///
    /// When setting up a webhook, Telegram lets you specify a `secret_token`.
    /// Telegram sends it back in every webhook request as the
    /// `X-Telegram-Bot-Api-Secret-Token` header.  We verify using
    /// constant-time comparison.
    #[allow(dead_code)]
    pub fn verify_webhook_secret(&self, header_value: &str, expected_secret: &str) -> bool {
        let a = header_value.as_bytes();
        let b = expected_secret.as_bytes();
        a.len() == b.len() && a.ct_eq(b).into()
    }

    /// Convert a Telegram update into an NgenOrca Message.
    pub(crate) fn telegram_message_to_ngenorca(update: &TelegramMessage) -> Option<Message> {
        let user = update.from.as_ref()?;
        let text = update.text.as_deref().unwrap_or("");
        if text.is_empty() {
            return None;
        }

        let user_display = user.username.as_deref().unwrap_or(&user.first_name);

        Some(Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId(format!("telegram:{}", user.id))),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(), // Will be resolved by the gateway
            channel: ChannelId(update.chat.id.to_string()),
            channel_kind: ChannelKind::Telegram,
            direction: Direction::Inbound,
            content: Content::Text(text.to_string()),
            metadata: serde_json::json!({
                "telegram_user_id": user.id,
                "telegram_username": user_display,
                "telegram_chat_id": update.chat.id,
                "telegram_message_id": update.message_id,
            }),
        })
    }

    /// Send a text message to a Telegram chat.
    async fn send_text(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i64>,
    ) -> ngenorca_core::Result<()> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        if let Some(reply_id) = reply_to {
            body["reply_to_message_id"] = serde_json::json!(reply_id);
        }

        let resp = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Telegram API error: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ngenorca_core::Error::Other(format!(
                "Telegram sendMessage failed ({status}): {text}"
            )));
        }

        Ok(())
    }

    /// Fetch updates via long-polling.
    #[allow(dead_code)]
    async fn get_updates(&self, offset: i64) -> ngenorca_core::Result<Vec<TelegramUpdate>> {
        let body = serde_json::json!({
            "offset": offset,
            "timeout": POLL_TIMEOUT_SECS,
            "allowed_updates": ["message"],
        });

        let resp = self
            .client
            .post(self.api_url("getUpdates"))
            .json(&body)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Telegram poll error: {e}")))?;

        let api_resp: TelegramApiResponse<Vec<TelegramUpdate>> = resp
            .json()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Telegram parse error: {e}")))?;

        if !api_resp.ok {
            return Err(ngenorca_core::Error::Other(format!(
                "Telegram API error: {}",
                api_resp.description.unwrap_or_default()
            )));
        }

        Ok(api_resp.result.unwrap_or_default())
    }

    /// Parse a raw Telegram webhook body into NgenOrca Messages.
    ///
    /// Expects a `TelegramUpdate` JSON. Returns a single-element vec if the
    /// update contains a valid text message, empty otherwise.
    pub(crate) fn parse_webhook_messages(body: &[u8]) -> Vec<Message> {
        let update: TelegramUpdate = match serde_json::from_slice(body) {
            Ok(u) => u,
            Err(_) => return Vec::new(),
        };

        let tg_msg = match update.message {
            Some(ref m) => m,
            None => return Vec::new(),
        };

        match Self::telegram_message_to_ngenorca(tg_msg) {
            Some(msg) => vec![msg],
            None => Vec::new(),
        }
    }
}

#[async_trait]
impl Plugin for TelegramAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-telegram".into()),
            name: "Telegram".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![
                Permission::ReadMessages,
                Permission::WriteMessages,
                Permission::Network,
            ],
            api_version: API_VERSION,
            description: "Telegram Bot adapter (polling + webhook)".into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        info!(
            polling = self.polling,
            webhook = ?self.webhook_url,
            allowed_users = self.allowed_users.len(),
            "Telegram adapter initialized"
        );
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        // Outbound: send response back to Telegram.
        if message.direction == Direction::Outbound {
            let chat_id: i64 = message
                .channel
                .0
                .parse()
                .map_err(|_| ngenorca_core::Error::Other("Invalid Telegram chat_id".into()))?;

            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };

            let reply_to = message
                .metadata
                .get("telegram_message_id")
                .and_then(|v| v.as_i64());

            self.send_text(chat_id, &text, reply_to).await?;
        }

        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // Quick API check: call getMe.
        let resp = self
            .client
            .get(self.api_url("getMe"))
            .send()
            .await
            .map_err(|e| {
                ngenorca_core::Error::Other(format!("Telegram health check failed: {e}"))
            })?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ngenorca_core::Error::Other(
                "Telegram API getMe returned non-200".into(),
            ))
        }
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        info!("Telegram adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        if !self.polling {
            // Webhook mode: the gateway receives POSTs from Telegram.
            // The webhook URL should be configured and registered with setWebhook.
            if let Some(url) = &self.webhook_url {
                let body = serde_json::json!({
                    "url": url,
                    "allowed_updates": ["message"],
                });
                let resp = self
                    .client
                    .post(self.api_url("setWebhook"))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| {
                        ngenorca_core::Error::Other(format!("Failed to set Telegram webhook: {e}"))
                    })?;

                if !resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(ngenorca_core::Error::Other(format!(
                        "Telegram setWebhook failed: {text}"
                    )));
                }

                info!(url = %url, "Telegram webhook configured");
            }
            return Ok(());
        }

        // Long-polling mode.
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let last_update_id = self.last_update_id.clone();
        let sender = self
            .sender
            .clone()
            .ok_or_else(|| ngenorca_core::Error::Other("Sender not initialized".into()))?;
        let token = self.token.clone();
        let allowed_users = self.allowed_users.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            info!("Telegram long-polling started");

            while running.load(Ordering::SeqCst) {
                let offset = last_update_id.load(Ordering::SeqCst) + 1;

                let body = serde_json::json!({
                    "offset": offset,
                    "timeout": POLL_TIMEOUT_SECS,
                    "allowed_updates": ["message"],
                });

                let result = client
                    .post(format!("{}/bot{}/getUpdates", TELEGRAM_API_BASE, token))
                    .json(&body)
                    .send()
                    .await;

                match result {
                    Ok(resp) => {
                        match resp
                            .json::<TelegramApiResponse<Vec<TelegramUpdate>>>()
                            .await
                        {
                            Ok(api_resp) if api_resp.ok => {
                                for update in api_resp.result.unwrap_or_default() {
                                    last_update_id.store(update.update_id, Ordering::SeqCst);

                                    if let Some(msg) = &update.message {
                                        // Check user permission.
                                        if let Some(user) = &msg.from
                                            && !allowed_users.is_empty()
                                            && !allowed_users.contains(&user.id)
                                        {
                                            debug!(
                                                user_id = user.id,
                                                "Telegram: ignoring message from unauthorized user"
                                            );
                                            continue;
                                        }

                                        if let Some(ngen_msg) =
                                            TelegramAdapter::telegram_message_to_ngenorca(msg)
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
                                                    "Failed to send Telegram event to bus"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(api_resp) => {
                                warn!(
                                    description = ?api_resp.description,
                                    "Telegram API returned error"
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to parse Telegram response");
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Telegram polling request failed");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }

            info!("Telegram long-polling stopped");
        });

        Ok(())
    }

    async fn send_message(&self, message: &Message) -> ngenorca_core::Result<()> {
        self.handle_message(message).await?;
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Telegram
    }
}

// ─── Telegram API Types ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TelegramUpdate {
    #[allow(dead_code)]
    pub(crate) update_id: i64,
    pub(crate) message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TelegramMessage {
    pub(crate) message_id: i64,
    pub(crate) from: Option<TelegramUser>,
    pub(crate) chat: TelegramChat,
    pub(crate) text: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) date: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TelegramUser {
    pub(crate) id: i64,
    pub(crate) first_name: String,
    #[serde(default)]
    pub(crate) username: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) is_bot: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TelegramChat {
    pub(crate) id: i64,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub(crate) chat_type: String,
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter() -> TelegramAdapter {
        TelegramAdapter::new(
            "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11".into(),
            true,
            None,
            vec![],
        )
    }

    #[test]
    fn manifest_is_channel_adapter() {
        let adapter = make_adapter();
        let manifest = adapter.manifest();
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
        assert_eq!(manifest.name, "Telegram");
    }

    #[test]
    fn channel_kind_is_telegram() {
        let adapter = make_adapter();
        assert_eq!(adapter.channel_kind(), ChannelKind::Telegram);
    }

    #[test]
    fn api_url_formats_correctly() {
        let adapter = make_adapter();
        let url = adapter.api_url("getMe");
        assert!(url.starts_with("https://api.telegram.org/bot"));
        assert!(url.ends_with("/getMe"));
    }

    #[test]
    fn user_allowed_when_list_empty() {
        let adapter = make_adapter();
        assert!(adapter.is_user_allowed(12345));
    }

    #[test]
    fn user_allowed_when_in_list() {
        let adapter = TelegramAdapter::new("tok".into(), true, None, vec![100, 200]);
        assert!(adapter.is_user_allowed(100));
        assert!(!adapter.is_user_allowed(300));
    }

    #[test]
    fn telegram_message_to_ngenorca_converts() {
        let tg_msg = TelegramMessage {
            message_id: 42,
            from: Some(TelegramUser {
                id: 12345,
                first_name: "Test".into(),
                username: Some("testuser".into()),
                is_bot: false,
            }),
            chat: TelegramChat {
                id: 67890,
                chat_type: "private".into(),
            },
            text: Some("Hello, NgenOrca!".into()),
            date: 0,
        };

        let msg = TelegramAdapter::telegram_message_to_ngenorca(&tg_msg).unwrap();
        assert_eq!(msg.user_id, Some(UserId("telegram:12345".into())));
        assert_eq!(msg.channel_kind, ChannelKind::Telegram);
        assert_eq!(msg.direction, Direction::Inbound);
        match &msg.content {
            Content::Text(text) => assert_eq!(text, "Hello, NgenOrca!"),
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn telegram_message_without_text_returns_none() {
        let tg_msg = TelegramMessage {
            message_id: 1,
            from: Some(TelegramUser {
                id: 1,
                first_name: "T".into(),
                username: None,
                is_bot: false,
            }),
            chat: TelegramChat {
                id: 1,
                chat_type: "private".into(),
            },
            text: None,
            date: 0,
        };

        assert!(TelegramAdapter::telegram_message_to_ngenorca(&tg_msg).is_none());
    }

    #[test]
    fn telegram_message_without_user_returns_none() {
        let tg_msg = TelegramMessage {
            message_id: 1,
            from: None,
            chat: TelegramChat {
                id: 1,
                chat_type: "private".into(),
            },
            text: Some("hi".into()),
            date: 0,
        };

        assert!(TelegramAdapter::telegram_message_to_ngenorca(&tg_msg).is_none());
    }
}
