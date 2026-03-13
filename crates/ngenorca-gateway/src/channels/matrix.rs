//! Matrix adapter — Client–Server API (v3) integration.
//!
//! Connects to any Matrix homeserver and long-polls `/sync` for incoming
//! messages.  Supports auto-joining rooms on invite.  End-to-end encryption
//! is **not** handled here — plaintext rooms only (E2EE would require
//! `vodozemac` / `matrix-sdk-crypto`).
//!
//! ## Receive path
//! 1. `GET /_matrix/client/v3/sync?timeout=30000&since={since}`
//! 2. For each room in `rooms.join`, iterate `timeline.events`.
//! 3. Filter for `m.room.message` with `msgtype: m.text`.
//! 4. Convert to NgenOrca `Message` and push to event bus.
//!
//! ## Send path
//! `PUT /_matrix/client/v3/rooms/{roomId}/send/m.room.message/{txnId}`

use async_trait::async_trait;
use ngenorca_core::ChannelKind;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::message::{Content, Direction, Message};
use ngenorca_core::plugin::{API_VERSION, Permission, PluginKind, PluginManifest};
use ngenorca_core::types::{ChannelId, EventId, PluginId, SessionId, TrustLevel, UserId};
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext, flume_like};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

const SYNC_TIMEOUT_MS: u64 = 30_000;

/// Matrix adapter.
pub struct MatrixAdapter {
    homeserver: String,
    user_id: String,
    access_token: String,
    #[allow(dead_code)]
    device_id: Option<String>,
    auto_join: bool,
    #[allow(dead_code)]
    encrypted: bool,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    client: reqwest::Client,
    /// Next batch token for incremental sync.
    since: Arc<Mutex<Option<String>>>,
}

impl MatrixAdapter {
    pub fn new(
        homeserver: String,
        user_id: String,
        access_token: String,
        device_id: Option<String>,
        auto_join: bool,
        encrypted: bool,
    ) -> Self {
        Self {
            homeserver,
            user_id,
            access_token,
            device_id,
            auto_join,
            encrypted,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(SYNC_TIMEOUT_MS / 1000 + 15))
                .build()
                .unwrap_or_default(),
            since: Arc::new(Mutex::new(None)),
        }
    }

    /// Build a URL for a Matrix CS API endpoint.
    fn api_url(&self, path: &str) -> String {
        let base = self.homeserver.trim_end_matches('/');
        format!("{base}/_matrix/client/v3{path}")
    }

    /// Auto-join a room (used when we're invited).
    #[allow(dead_code)]
    async fn join_room(&self, room_id: &str) -> ngenorca_core::Result<()> {
        let url = self.api_url(&format!("/join/{}", urlencoding::encode(room_id)));
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Matrix join: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            warn!(room = room_id, body = text, "Matrix join failed");
        } else {
            info!(room = room_id, "Auto-joined Matrix room");
        }
        Ok(())
    }

    /// Send a text message to a Matrix room.
    async fn send_text(&self, room_id: &str, text: &str) -> ngenorca_core::Result<()> {
        let txn_id = uuid::Uuid::now_v7().to_string();
        let url = self.api_url(&format!(
            "/rooms/{}/send/m.room.message/{txn_id}",
            urlencoding::encode(room_id)
        ));

        let body = serde_json::json!({
            "msgtype": "m.text",
            "body": text,
        });

        let resp = self
            .client
            .put(&url)
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Matrix send: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(ngenorca_core::Error::Other(format!(
                "Matrix send failed ({status}): {err}"
            )));
        }

        Ok(())
    }

    /// Convert a Matrix timeline event into an NgenOrca Message.
    fn matrix_event_to_message(event: &MatrixTimelineEvent, room_id: &str) -> Option<Message> {
        if event.event_type != "m.room.message" {
            return None;
        }

        let body = event.content.get("body")?.as_str()?;
        let msgtype = event.content.get("msgtype")?.as_str()?;
        if msgtype != "m.text" || body.is_empty() {
            return None;
        }

        Some(Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: Some(UserId(format!("matrix:{}", event.sender))),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId(room_id.to_string()),
            channel_kind: ChannelKind::Matrix,
            direction: Direction::Inbound,
            content: Content::Text(body.to_string()),
            metadata: serde_json::json!({
                "matrix_sender": event.sender,
                "matrix_room_id": room_id,
                "matrix_event_id": event.event_id,
            }),
        })
    }
}

#[async_trait]
impl Plugin for MatrixAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-matrix".into()),
            name: "Matrix".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![
                Permission::ReadMessages,
                Permission::WriteMessages,
                Permission::Network,
            ],
            api_version: API_VERSION,
            description: "Matrix homeserver adapter (CS API)".into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        info!(
            homeserver = %self.homeserver,
            user_id = %self.user_id,
            encrypted = self.encrypted,
            auto_join = self.auto_join,
            "Matrix adapter initialized (token length: {})",
            self.access_token.len()
        );
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        if message.direction == Direction::Outbound {
            let room_id = &message.channel.0;
            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };
            self.send_text(room_id, &text).await?;
        }
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        let url = {
            let base = self.homeserver.trim_end_matches('/');
            format!("{base}/_matrix/client/versions")
        };
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Matrix health: {e}")))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(ngenorca_core::Error::Other(
                "Matrix homeserver returned non-200 for /versions".into(),
            ))
        }
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        info!("Matrix adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for MatrixAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        self.running.store(true, Ordering::SeqCst);

        let running = self.running.clone();
        let sender = self
            .sender
            .clone()
            .ok_or_else(|| ngenorca_core::Error::Other("Sender not initialized".into()))?;
        let client = self.client.clone();
        let access_token = self.access_token.clone();
        let homeserver = self.homeserver.clone();
        let own_user_id = self.user_id.clone();
        let auto_join = self.auto_join;
        let since = self.since.clone();

        tokio::spawn(async move {
            info!("Matrix /sync long-poll started");

            // Do an initial sync with timeout=0 to get the `since` token
            // without processing old history.
            let base = homeserver.trim_end_matches('/');
            {
                let url = format!(
                    "{base}/_matrix/client/v3/sync?timeout=0&filter={{\"room\":{{\"timeline\":{{\"limit\":0}}}}}}"
                );
                if let Ok(resp) = client.get(&url).bearer_auth(&access_token).send().await
                    && let Ok(body) = resp.json::<serde_json::Value>().await
                    && let Some(token) = body["next_batch"].as_str()
                {
                    *since.lock().await = Some(token.to_string());
                    debug!(since = token, "Matrix initial sync complete");
                }
            }

            while running.load(Ordering::SeqCst) {
                let since_token = since.lock().await.clone();
                let mut url = format!("{base}/_matrix/client/v3/sync?timeout={SYNC_TIMEOUT_MS}");
                if let Some(ref token) = since_token {
                    url.push_str(&format!("&since={token}"));
                }

                let result = client.get(&url).bearer_auth(&access_token).send().await;
                match result {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<MatrixSyncResponse>().await {
                            Ok(sync) => {
                                *since.lock().await = Some(sync.next_batch.clone());

                                // Process invites (auto-join).
                                if auto_join {
                                    for room_id in sync.rooms.invite.keys() {
                                        let join_url = format!(
                                            "{base}/_matrix/client/v3/join/{}",
                                            urlencoding::encode(room_id)
                                        );
                                        let _ = client
                                            .post(&join_url)
                                            .bearer_auth(&access_token)
                                            .json(&serde_json::json!({}))
                                            .send()
                                            .await;
                                        info!(room = room_id, "Auto-joined Matrix room");
                                    }
                                }

                                // Process joined room timeline events.
                                for (room_id, room_data) in &sync.rooms.join {
                                    for event in &room_data.timeline.events {
                                        // Skip our own messages.
                                        if event.sender == own_user_id {
                                            continue;
                                        }
                                        if let Some(ngen_msg) =
                                            MatrixAdapter::matrix_event_to_message(event, room_id)
                                        {
                                            let bus_event = Event {
                                                id: EventId::new(),
                                                timestamp: chrono::Utc::now(),
                                                session_id: Some(ngen_msg.session_id.clone()),
                                                user_id: ngen_msg.user_id.clone(),
                                                payload: EventPayload::Message(ngen_msg),
                                            };
                                            if let Err(e) = sender.send(bus_event) {
                                                error!(
                                                    error = %e,
                                                    "Failed to send Matrix event to bus"
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to parse Matrix /sync");
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            }
                        }
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        warn!(status = %status, "Matrix /sync returned error");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                    Err(e) => {
                        warn!(error = %e, "Matrix /sync request failed");
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }

            info!("Matrix /sync polling stopped");
        });

        Ok(())
    }

    async fn send_message(&self, message: &Message) -> ngenorca_core::Result<()> {
        self.handle_message(message).await?;
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Matrix
    }
}

// ─── Matrix CS API Types ────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct MatrixSyncResponse {
    next_batch: String,
    #[serde(default)]
    rooms: MatrixSyncRooms,
}

#[derive(Debug, Default, Deserialize)]
struct MatrixSyncRooms {
    #[serde(default)]
    join: std::collections::HashMap<String, MatrixJoinedRoom>,
    #[serde(default)]
    invite: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct MatrixJoinedRoom {
    #[serde(default)]
    timeline: MatrixTimeline,
}

#[derive(Debug, Default, Deserialize)]
struct MatrixTimeline {
    #[serde(default)]
    events: Vec<MatrixTimelineEvent>,
}

#[derive(Debug, Deserialize)]
struct MatrixTimelineEvent {
    #[serde(rename = "type")]
    event_type: String,
    sender: String,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    content: serde_json::Value,
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter() -> MatrixAdapter {
        MatrixAdapter::new(
            "https://matrix.example.org".into(),
            "@bot:example.org".into(),
            "syt_token_abc".into(),
            None,
            true,
            false,
        )
    }

    #[test]
    fn matrix_manifest() {
        let adapter = make_adapter();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Matrix");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[test]
    fn matrix_channel_kind() {
        let adapter = make_adapter();
        assert_eq!(adapter.channel_kind(), ChannelKind::Matrix);
    }

    #[test]
    fn api_url_formats_correctly() {
        let adapter = make_adapter();
        let url = adapter.api_url("/sync");
        assert_eq!(url, "https://matrix.example.org/_matrix/client/v3/sync");
    }

    #[test]
    fn matrix_event_to_message_converts_text() {
        let event = MatrixTimelineEvent {
            event_type: "m.room.message".into(),
            sender: "@user:example.org".into(),
            event_id: Some("$abc123".into()),
            content: serde_json::json!({
                "msgtype": "m.text",
                "body": "Hello from Matrix!"
            }),
        };
        let msg = MatrixAdapter::matrix_event_to_message(&event, "!room:example.org").unwrap();
        assert_eq!(msg.user_id, Some(UserId("matrix:@user:example.org".into())));
        assert_eq!(msg.channel, ChannelId("!room:example.org".into()));
        assert_eq!(msg.channel_kind, ChannelKind::Matrix);
        match &msg.content {
            Content::Text(t) => assert_eq!(t, "Hello from Matrix!"),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn matrix_event_skips_non_text() {
        let event = MatrixTimelineEvent {
            event_type: "m.room.member".into(),
            sender: "@user:example.org".into(),
            event_id: None,
            content: serde_json::json!({}),
        };
        assert!(MatrixAdapter::matrix_event_to_message(&event, "!room:x").is_none());
    }

    #[test]
    fn matrix_event_skips_empty_body() {
        let event = MatrixTimelineEvent {
            event_type: "m.room.message".into(),
            sender: "@user:example.org".into(),
            event_id: None,
            content: serde_json::json!({
                "msgtype": "m.text",
                "body": ""
            }),
        };
        assert!(MatrixAdapter::matrix_event_to_message(&event, "!room:x").is_none());
    }
}
