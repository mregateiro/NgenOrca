//! Discord Bot adapter via the Discord Gateway (WebSocket) + REST API.
//!
//! ## Receive path
//! Connects to `wss://gateway.discord.gg/?v=10&encoding=json`, sends IDENTIFY
//! with the bot token, then processes MESSAGE_CREATE events.  Heartbeats are
//! maintained in a background task.
//!
//! ## Send path
//! `POST https://discord.com/api/v10/channels/{channel_id}/messages`

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{flume_like, ChannelAdapter, Plugin, PluginContext};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite;
use tracing::{debug, error, info, warn};

const DISCORD_API: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Discord Bot adapter using raw Gateway WebSocket.
pub struct DiscordAdapter {
    bot_token: String,
    guild_ids: Vec<String>,
    command_prefix: Option<String>,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    client: reqwest::Client,
    /// Shared sequence number for heartbeats.
    sequence: Arc<Mutex<Option<u64>>>,
}

impl DiscordAdapter {
    pub fn new(
        bot_token: String,
        guild_ids: Vec<String>,
        command_prefix: Option<String>,
    ) -> Self {
        Self {
            bot_token,
            guild_ids,
            command_prefix,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::new(),
            sequence: Arc::new(Mutex::new(None)),
        }
    }

    /// Send a text message to a Discord channel via REST.
    async fn send_text(&self, channel_id: &str, text: &str) -> ngenorca_core::Result<()> {
        let url = format!("{DISCORD_API}/channels/{channel_id}/messages");
        let body = serde_json::json!({ "content": text });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&body)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Discord send: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(ngenorca_core::Error::Other(format!(
                "Discord send failed ({status}): {err}"
            )));
        }
        debug!(channel = channel_id, "Discord message sent");
        Ok(())
    }

    /// Convert a Discord MESSAGE_CREATE event into an NgenOrca Message.
    fn gateway_message_to_ngenorca(
        data: &DiscordMessageData,
        command_prefix: Option<&str>,
        guild_ids: &[String],
    ) -> Option<Message> {
        // Skip messages from bots.
        if data.author.bot.unwrap_or(false) {
            return None;
        }

        let text = &data.content;
        if text.is_empty() {
            return None;
        }

        // If guild_ids filter is non-empty, only accept messages from those guilds.
        if !guild_ids.is_empty() {
            if let Some(ref gid) = data.guild_id {
                if !guild_ids.iter().any(|g| g == gid) {
                    return None;
                }
            } else {
                // DM — allow through.
            }
        }

        // If a command prefix is set, only handle messages that start with it.
        let body = if let Some(prefix) = command_prefix {
            if let Some(stripped) = text.strip_prefix(prefix) {
                stripped.trim().to_string()
            } else {
                return None;
            }
        } else {
            text.clone()
        };

        if body.is_empty() {
            return None;
        }

        Some(Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId(format!("discord:{}", data.author.id))),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId(data.channel_id.clone()),
            channel_kind: ChannelKind::Discord,
            direction: Direction::Inbound,
            content: Content::Text(body),
            metadata: serde_json::json!({
                "discord_message_id": data.id,
                "discord_channel_id": data.channel_id,
                "discord_guild_id": data.guild_id,
                "discord_author_username": data.author.username,
            }),
        })
    }
}

#[async_trait]
impl Plugin for DiscordAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-discord".into()),
            name: "Discord".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![
                Permission::ReadMessages,
                Permission::WriteMessages,
                Permission::Network,
            ],
            api_version: API_VERSION,
            description: "Discord Bot adapter via Gateway API".into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        info!(
            guilds = ?self.guild_ids,
            prefix = ?self.command_prefix,
            "Discord adapter initialized (token length: {})",
            self.bot_token.len()
        );
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        if message.direction == Direction::Outbound {
            let channel_id = &message.channel.0;
            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };
            self.send_text(channel_id, &text).await?;
        }
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        let url = format!("{DISCORD_API}/users/@me");
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Discord health: {e}")))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ngenorca_core::Error::Other(
                "Discord health check: token invalid".into(),
            ))
        }
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        info!("Discord adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for DiscordAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        let sender = self
            .sender
            .clone()
            .ok_or_else(|| ngenorca_core::Error::Other("Discord: not initialized".into()))?;

        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let token = self.bot_token.clone();
        let sequence = self.sequence.clone();
        let guild_ids = self.guild_ids.clone();
        let command_prefix = self.command_prefix.clone();

        tokio::spawn(async move {
            while running.load(Ordering::SeqCst) {
                info!("Discord: connecting to Gateway …");
                match tokio_tungstenite::connect_async(DISCORD_GATEWAY_URL).await {
                    Ok((ws_stream, _)) => {
                        let (mut write, mut read) = ws_stream.split();

                        // 1. Receive HELLO (op 10) to get heartbeat_interval.
                        let heartbeat_interval = match read.next().await {
                            Some(Ok(tungstenite::Message::Text(text))) => {
                                let payload: serde_json::Value =
                                    serde_json::from_str(&text).unwrap_or_default();
                                payload["d"]["heartbeat_interval"]
                                    .as_u64()
                                    .unwrap_or(41_250)
                            }
                            _ => 41_250,
                        };

                        // 2. Send IDENTIFY (op 2).
                        let identify = serde_json::json!({
                            "op": 2,
                            "d": {
                                "token": token,
                                "intents": 33281, // GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT | DIRECT_MESSAGES
                                "properties": {
                                    "os": "linux",
                                    "browser": "ngenorca",
                                    "device": "ngenorca"
                                }
                            }
                        });
                        if let Err(e) = write
                            .send(tungstenite::Message::Text(identify.to_string().into()))
                            .await
                        {
                            error!(error = %e, "Discord: IDENTIFY failed");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            continue;
                        }

                        // 3. Spawn heartbeat task.
                        let hb_running = running.clone();
                        let hb_seq = sequence.clone();
                        let (hb_tx, mut hb_rx) =
                            tokio::sync::mpsc::unbounded_channel::<String>();

                        let hb_handle = tokio::spawn(async move {
                            let interval =
                                std::time::Duration::from_millis(heartbeat_interval);
                            while hb_running.load(Ordering::SeqCst) {
                                tokio::time::sleep(interval).await;
                                let seq = hb_seq.lock().await;
                                let hb = serde_json::json!({ "op": 1, "d": *seq });
                                if hb_tx.send(hb.to_string()).is_err() {
                                    break;
                                }
                            }
                        });

                        // 4. Main read loop.
                        loop {
                            tokio::select! {
                                msg = read.next() => {
                                    match msg {
                                        Some(Ok(tungstenite::Message::Text(text))) => {
                                            let payload: serde_json::Value =
                                                serde_json::from_str(&text).unwrap_or_default();

                                            // Update sequence number.
                                            if let Some(s) = payload["s"].as_u64() {
                                                let mut seq = sequence.lock().await;
                                                *seq = Some(s);
                                            }

                                            let op = payload["op"].as_u64().unwrap_or(0);
                                            let t = payload["t"].as_str().unwrap_or("");

                                            match (op, t) {
                                                (0, "MESSAGE_CREATE") => {
                                                    if let Ok(data) =
                                                        serde_json::from_value::<DiscordMessageData>(
                                                            payload["d"].clone(),
                                                        )
                                                        && let Some(ngen_msg) =
                                                            DiscordAdapter::gateway_message_to_ngenorca(
                                                                &data,
                                                                command_prefix.as_deref(),
                                                                &guild_ids,
                                                            )
                                                        {
                                                            let event = Event {
                                                                id: EventId::new(),
                                                                timestamp: chrono::Utc::now(),
                                                                session_id: Some(
                                                                    ngen_msg.session_id.clone(),
                                                                ),
                                                                user_id: ngen_msg
                                                                    .user_id
                                                                    .clone(),
                                                                payload:
                                                                    EventPayload::Message(
                                                                        ngen_msg,
                                                                    ),
                                                            };
                                                            if let Err(e) =
                                                                sender.send(event)
                                                            {
                                                                error!(
                                                                    error = %e,
                                                                    "Discord: failed to send event"
                                                                );
                                                            }
                                                        }
                                                }
                                                (11, _) => {
                                                    // Heartbeat ACK — good.
                                                    debug!("Discord: heartbeat ACK");
                                                }
                                                (1, _) => {
                                                    // Server requests heartbeat.
                                                    let seq = sequence.lock().await;
                                                    let hb = serde_json::json!({ "op": 1, "d": *seq });
                                                    if let Err(e) = write
                                                        .send(tungstenite::Message::Text(
                                                            hb.to_string().into(),
                                                        ))
                                                        .await
                                                    {
                                                        warn!(error = %e, "Discord: heartbeat send");
                                                    }
                                                }
                                                (7, _) => {
                                                    // Reconnect requested.
                                                    info!("Discord: reconnect requested");
                                                    break;
                                                }
                                                (9, _) => {
                                                    // Invalid session.
                                                    warn!("Discord: invalid session, reconnecting");
                                                    break;
                                                }
                                                _ => {
                                                    debug!(op = op, t = t, "Discord: gateway event");
                                                }
                                            }
                                        }
                                        Some(Ok(tungstenite::Message::Close(_))) => {
                                            info!("Discord: gateway closed");
                                            break;
                                        }
                                        Some(Err(e)) => {
                                            error!(error = %e, "Discord: read error");
                                            break;
                                        }
                                        None => break,
                                        _ => {}
                                    }
                                }
                                Some(hb_text) = hb_rx.recv() => {
                                    if let Err(e) = write
                                        .send(tungstenite::Message::Text(hb_text.into()))
                                        .await
                                    {
                                        warn!(error = %e, "Discord: heartbeat write failed");
                                        break;
                                    }
                                }
                            }

                            if !running.load(Ordering::SeqCst) {
                                break;
                            }
                        }

                        hb_handle.abort();
                    }
                    Err(e) => {
                        error!(error = %e, "Discord: gateway connect error");
                    }
                }

                if running.load(Ordering::SeqCst) {
                    info!("Discord: reconnecting in 5 s …");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        });

        Ok(())
    }

    async fn send_message(&self, message: &Message) -> ngenorca_core::Result<()> {
        self.handle_message(message).await?;
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Discord
    }
}

// ─── Discord Gateway Types ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DiscordMessageData {
    id: String,
    channel_id: String,
    #[serde(default)]
    guild_id: Option<String>,
    content: String,
    author: DiscordAuthor,
}

#[derive(Debug, Deserialize)]
struct DiscordAuthor {
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    bot: Option<bool>,
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter() -> DiscordAdapter {
        DiscordAdapter::new("test-token".into(), vec!["guild1".into()], Some("!".into()))
    }

    fn make_msg(content: &str, bot: bool, guild_id: Option<&str>) -> DiscordMessageData {
        DiscordMessageData {
            id: "msg123".into(),
            channel_id: "chan456".into(),
            guild_id: guild_id.map(String::from),
            content: content.into(),
            author: DiscordAuthor {
                id: "user789".into(),
                username: "tester".into(),
                bot: Some(bot),
            },
        }
    }

    #[test]
    fn discord_manifest() {
        let adapter = make_adapter();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Discord");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[test]
    fn discord_channel_kind() {
        let adapter = make_adapter();
        assert_eq!(adapter.channel_kind(), ChannelKind::Discord);
    }

    #[test]
    fn message_converts_with_prefix() {
        let data = make_msg("!hello world", false, Some("guild1"));
        let msg =
            DiscordAdapter::gateway_message_to_ngenorca(&data, Some("!"), &["guild1".into()])
                .unwrap();
        assert_eq!(msg.user_id, Some(UserId("discord:user789".into())));
        match &msg.content {
            Content::Text(t) => assert_eq!(t, "hello world"),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn message_without_prefix_filtered() {
        let data = make_msg("hello world", false, Some("guild1"));
        assert!(
            DiscordAdapter::gateway_message_to_ngenorca(&data, Some("!"), &["guild1".into()])
                .is_none()
        );
    }

    #[test]
    fn bot_messages_skipped() {
        let data = make_msg("!hello", true, Some("guild1"));
        assert!(
            DiscordAdapter::gateway_message_to_ngenorca(&data, Some("!"), &["guild1".into()])
                .is_none()
        );
    }

    #[test]
    fn unknown_guild_filtered() {
        let data = make_msg("!hello", false, Some("guild999"));
        assert!(
            DiscordAdapter::gateway_message_to_ngenorca(&data, Some("!"), &["guild1".into()])
                .is_none()
        );
    }

    #[test]
    fn dm_allowed_through() {
        let data = make_msg("!hello", false, None);
        let msg =
            DiscordAdapter::gateway_message_to_ngenorca(&data, Some("!"), &["guild1".into()]);
        assert!(msg.is_some());
    }

    #[test]
    fn no_prefix_passes_all() {
        let data = make_msg("normal message", false, Some("guild1"));
        let msg =
            DiscordAdapter::gateway_message_to_ngenorca(&data, None, &["guild1".into()])
                .unwrap();
        match &msg.content {
            Content::Text(t) => assert_eq!(t, "normal message"),
            _ => panic!("Expected text"),
        }
    }
}
