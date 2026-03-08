//! Microsoft Teams adapter via the Azure Bot Framework REST API.
//!
//! ## Receive path (webhook)
//! Teams sends POST requests (Bot Framework Activities) to the webhook endpoint
//! (typically `/api/messages`).  The gateway registers the route and forwards
//! inbound activities here.
//!
//! ## Send path
//! To reply, the adapter POSTs an Activity to the `serviceUrl` provided in the
//! inbound activity, using an OAuth2 bearer token obtained from Azure AD.
//!
//! Token endpoint:
//! `POST https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token`
//! with `grant_type=client_credentials`, `scope=https://api.botframework.com/.default`,
//! `client_id` and `client_secret`.

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
use tokio::sync::Mutex;
use tracing::{debug, error, info};

/// Microsoft Teams adapter via Bot Framework.
pub struct TeamsAdapter {
    app_id: String,
    app_password: String,
    tenant_id: Option<String>,
    webhook_url: Option<String>,
    sender: Option<flume_like::Sender>,
    running: Arc<AtomicBool>,
    client: reqwest::Client,
    /// Cached OAuth2 bearer token plus its expiry.
    token_cache: Arc<Mutex<Option<CachedToken>>>,
}

struct CachedToken {
    access_token: String,
    expires_at: std::time::Instant,
}

impl TeamsAdapter {
    pub fn new(
        app_id: String,
        app_password: String,
        tenant_id: Option<String>,
        webhook_url: Option<String>,
    ) -> Self {
        Self {
            app_id,
            app_password,
            tenant_id,
            webhook_url,
            sender: None,
            running: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::new(),
            token_cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Obtain (or refresh) the Azure AD OAuth2 token.
    async fn get_token(&self) -> ngenorca_core::Result<String> {
        {
            let cache = self.token_cache.lock().await;
            if let Some(ref ct) = *cache
                && ct.expires_at > std::time::Instant::now() {
                    return Ok(ct.access_token.clone());
                }
        }

        let tenant = self.tenant_id.as_deref().unwrap_or("botframework.com");
        let url = format!(
            "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"
        );

        let params = [
            ("grant_type", "client_credentials"),
            ("client_id", &self.app_id),
            ("client_secret", &self.app_password),
            ("scope", "https://api.botframework.com/.default"),
        ];

        let resp = self
            .client
            .post(&url)
            .form(&params)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Teams OAuth: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ngenorca_core::Error::Other(format!(
                "Teams OAuth failed ({status}): {body}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Teams OAuth parse: {e}")))?;

        let access_token = body["access_token"]
            .as_str()
            .ok_or_else(|| ngenorca_core::Error::Other("Teams: no access_token".into()))?
            .to_string();

        let expires_in = body["expires_in"].as_u64().unwrap_or(3600);
        // Refresh 60 s before real expiry.
        let expires_at =
            std::time::Instant::now() + std::time::Duration::from_secs(expires_in.saturating_sub(60));

        let mut cache = self.token_cache.lock().await;
        *cache = Some(CachedToken {
            access_token: access_token.clone(),
            expires_at,
        });

        Ok(access_token)
    }

    /// Send a reply Activity back to Teams.
    async fn reply_to_activity(
        &self,
        service_url: &str,
        conversation_id: &str,
        text: &str,
    ) -> ngenorca_core::Result<()> {
        let token = self.get_token().await?;
        let url = format!(
            "{service_url}v3/conversations/{conversation_id}/activities"
        );

        let activity = serde_json::json!({
            "type": "message",
            "text": text,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&activity)
            .send()
            .await
            .map_err(|e| ngenorca_core::Error::Other(format!("Teams reply: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(ngenorca_core::Error::Other(format!(
                "Teams reply failed ({status}): {err}"
            )));
        }

        debug!(conversation_id = conversation_id, "Teams message sent");
        Ok(())
    }

    /// Convert a Bot Framework Activity into an NgenOrca `Message`.
    fn activity_to_message(activity: &BotFrameworkActivity) -> Option<Message> {
        if activity.activity_type.as_deref() != Some("message") {
            return None;
        }

        let text = activity.text.as_deref().unwrap_or("");
        if text.is_empty() {
            return None;
        }

        let user_id = activity
            .from
            .as_ref()
            .map(|f| UserId(format!("teams:{}", f.id)));

        let channel_id = activity
            .conversation
            .as_ref()
            .map(|c| c.id.clone())
            .unwrap_or_default();

        Some(Message {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            user_id: user_id.clone(),
            trust: TrustLevel::Channel,
            session_id: SessionId::new(),
            channel: ChannelId(channel_id.clone()),
            channel_kind: ChannelKind::Teams,
            direction: Direction::Inbound,
            content: Content::Text(text.to_string()),
            metadata: serde_json::json!({
                "teams_activity_id": activity.id,
                "teams_conversation_id": channel_id,
                "teams_service_url": activity.service_url,
                "teams_from_name": activity.from.as_ref().map(|f| &f.name),
            }),
        })
    }

    /// Process an inbound Bot Framework Activity (called from the webhook route).
    pub fn process_activity(
        activity: &BotFrameworkActivity,
        sender: &flume_like::Sender,
    ) {
        if let Some(ngen_msg) = TeamsAdapter::activity_to_message(activity) {
            let event = Event {
                id: EventId::new(),
                timestamp: chrono::Utc::now(),
                session_id: Some(ngen_msg.session_id.clone()),
                user_id: ngen_msg.user_id.clone(),
                payload: EventPayload::Message(ngen_msg),
            };
            if let Err(e) = sender.send(event) {
                error!(error = %e, "Failed to send Teams event to bus");
            }
        }
    }

    /// Webhook URL getter.
    pub fn webhook_url(&self) -> Option<&str> {
        self.webhook_url.as_deref()
    }
}

#[async_trait]
impl Plugin for TeamsAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-teams".into()),
            name: "Teams".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![
                Permission::ReadMessages,
                Permission::WriteMessages,
                Permission::Network,
            ],
            api_version: API_VERSION,
            description: "Microsoft Teams adapter via Bot Framework".into(),
        }
    }

    async fn init(&mut self, ctx: PluginContext) -> ngenorca_core::Result<()> {
        self.sender = Some(ctx.sender);
        info!(
            app_id = %self.app_id,
            has_tenant = self.tenant_id.is_some(),
            has_webhook = self.webhook_url.is_some(),
            "Teams adapter initialized"
        );
        Ok(())
    }

    async fn handle_message(&self, message: &Message) -> ngenorca_core::Result<Option<Message>> {
        if message.direction == Direction::Outbound {
            // Extract service_url and conversation_id from metadata.
            let service_url = message.metadata["teams_service_url"]
                .as_str()
                .unwrap_or("https://smba.trafficmanager.net/amer/");
            let conversation_id = &message.channel.0;
            let text = match &message.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };
            self.reply_to_activity(service_url, conversation_id, &text)
                .await?;
        }
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // Verify that we can obtain an OAuth2 token.
        self.get_token().await?;
        Ok(())
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        info!("Teams adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for TeamsAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // Teams is webhook-based — the Bot Framework POSTs Activities to our
        // endpoint.  The gateway registers the webhook route (e.g. `/api/messages`)
        // and forwards parsed activities to `process_activity`.
        self.running.store(true, Ordering::SeqCst);

        info!(
            webhook = ?self.webhook_url,
            app_id = %self.app_id,
            "Teams adapter listening for Bot Framework activity POSTs"
        );

        Ok(())
    }

    async fn send_message(&self, message: &Message) -> ngenorca_core::Result<()> {
        self.handle_message(message).await?;
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Teams
    }
}

// ─── Bot Framework Types ────────────────────────────────────────

/// Inbound Bot Framework Activity.
#[derive(Debug, Deserialize)]
pub struct BotFrameworkActivity {
    #[serde(default, rename = "type")]
    pub activity_type: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "serviceUrl")]
    pub service_url: Option<String>,
    #[serde(default)]
    pub from: Option<BotFrameworkAccount>,
    #[serde(default)]
    pub conversation: Option<BotFrameworkConversation>,
}

#[derive(Debug, Deserialize)]
pub struct BotFrameworkAccount {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct BotFrameworkConversation {
    #[serde(default)]
    pub id: String,
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adapter() -> TeamsAdapter {
        TeamsAdapter::new(
            "app-id-123".into(),
            "app-secret".into(),
            Some("tenant-456".into()),
            Some("https://example.com/api/messages".into()),
        )
    }

    fn make_activity(text: Option<&str>, from_id: &str) -> BotFrameworkActivity {
        BotFrameworkActivity {
            activity_type: Some("message".into()),
            id: Some("act-456".into()),
            text: text.map(String::from),
            service_url: Some("https://smba.trafficmanager.net/amer/".into()),
            from: Some(BotFrameworkAccount {
                id: from_id.into(),
                name: "Test User".into(),
            }),
            conversation: Some(BotFrameworkConversation {
                id: "conv-789".into(),
            }),
        }
    }

    #[test]
    fn teams_manifest() {
        let adapter = make_adapter();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Teams");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[test]
    fn teams_channel_kind() {
        let adapter = make_adapter();
        assert_eq!(adapter.channel_kind(), ChannelKind::Teams);
    }

    #[test]
    fn activity_converts_to_message() {
        let activity = make_activity(Some("Hello Teams!"), "user1");
        let msg = TeamsAdapter::activity_to_message(&activity).unwrap();
        assert_eq!(msg.user_id, Some(UserId("teams:user1".into())));
        assert_eq!(msg.channel_kind, ChannelKind::Teams);
        match &msg.content {
            Content::Text(t) => assert_eq!(t, "Hello Teams!"),
            _ => panic!("Expected text"),
        }
    }

    #[test]
    fn non_message_activity_returns_none() {
        let activity = BotFrameworkActivity {
            activity_type: Some("conversationUpdate".into()),
            id: None,
            text: Some("Hi".into()),
            service_url: None,
            from: None,
            conversation: None,
        };
        assert!(TeamsAdapter::activity_to_message(&activity).is_none());
    }

    #[test]
    fn empty_text_returns_none() {
        let activity = make_activity(Some(""), "user1");
        assert!(TeamsAdapter::activity_to_message(&activity).is_none());
    }

    #[test]
    fn no_text_returns_none() {
        let activity = make_activity(None, "user1");
        assert!(TeamsAdapter::activity_to_message(&activity).is_none());
    }

    #[test]
    fn process_activity_sends_event() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = flume_like::Sender::new(tx);

        let activity = make_activity(Some("Hello from Teams"), "user42");
        TeamsAdapter::process_activity(&activity, &sender);

        let event = rx.try_recv().expect("should have event");
        match event.payload {
            EventPayload::Message(msg) => {
                assert_eq!(msg.channel_kind, ChannelKind::Teams);
                match &msg.content {
                    Content::Text(t) => assert_eq!(t, "Hello from Teams"),
                    _ => panic!("Expected text"),
                }
            }
            _ => panic!("Expected message event"),
        }
    }
}
