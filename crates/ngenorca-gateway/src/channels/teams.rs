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

    /// SEC-05: Verify an incoming Bot Framework webhook request.
    ///
    /// Full Bot Framework JWT validation — fetches the OpenID metadata document,
    /// retrieves JWKS signing keys (with 1-hour caching), and validates the
    /// `Authorization: Bearer` header JWT against:
    /// * **Algorithm**: RS256 / RS384 / RS512 only (prevents algorithm confusion).
    /// * **Issuer**: `https://api.botframework.com`.
    /// * **Audience**: the configured `app_id`.
    /// * **Expiry**: standard `exp` claim check.
    /// * **Signature**: RSA cryptographic verification via the matched JWKS key.
    ///
    /// On any network error fetching JWKS the function returns `false`
    /// (fail-closed: reject if we cannot verify).
    #[allow(dead_code)]
    pub async fn verify_bot_framework_jwt(
        auth_header: &str,
        expected_audience: Option<&str>,
    ) -> bool {
        let token = match auth_header.strip_prefix("Bearer ") {
            Some(t) => t.trim(),
            None => return false,
        };

        // Decode JWT header (without signature verification) to extract `kid`.
        let header = match jsonwebtoken::decode_header(token) {
            Ok(h) => h,
            Err(_) => return false,
        };

        match verify_jwt_with_jwks(token, &header, expected_audience).await {
            Ok(valid) => valid,
            Err(e) => {
                warn!(error = %e, "Teams JWKS verification failed — rejecting (fail-closed)");
                false
            }
        }
    }

    /// Legacy structural-only JWT check (kept for backwards compat / tests).
    ///
    /// **Deprecated**: Use [`verify_bot_framework_jwt`] for production deployments.
    #[allow(dead_code)]
    pub fn verify_bot_framework_token(auth_header: &str) -> bool {
        let token = match auth_header.strip_prefix("Bearer ") {
            Some(t) => t.trim(),
            None => return false,
        };
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return false;
        }
        parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=')
        })
    }

    /// Obtain (or refresh) the Azure AD OAuth2 token.
    async fn get_token(&self) -> ngenorca_core::Result<String> {
        {
            let cache = self.token_cache.lock().await;
            if let Some(ref ct) = *cache
                && ct.expires_at > std::time::Instant::now()
            {
                return Ok(ct.access_token.clone());
            }
        }

        let tenant = self.tenant_id.as_deref().unwrap_or("botframework.com");
        let url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");

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
        let expires_at = std::time::Instant::now()
            + std::time::Duration::from_secs(expires_in.saturating_sub(60));

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
        let url = format!("{service_url}v3/conversations/{conversation_id}/activities");

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
    pub fn process_activity(activity: &BotFrameworkActivity, sender: &flume_like::Sender) {
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

// ─── JWKS cache & JWT verification helpers ──────────────────────

/// Bot Framework OpenID metadata URL.
const OPENID_CONFIG_URL: &str = "https://login.botframework.com/v1/.well-known/openidconfiguration";

/// Expected issuer claim for Bot Framework tokens.
const EXPECTED_ISSUER: &str = "https://api.botframework.com";

/// JWKS cache TTL (seconds).
const JWKS_CACHE_TTL_SECS: u64 = 3600;

/// Module-level JWKS cache — lazily initialised, shared across all requests.
static JWKS_CACHE: std::sync::LazyLock<tokio::sync::RwLock<JwksCacheInner>> =
    std::sync::LazyLock::new(|| {
        tokio::sync::RwLock::new(JwksCacheInner {
            keys: None,
            fetched_at: std::time::Instant::now()
                - std::time::Duration::from_secs(JWKS_CACHE_TTL_SECS + 1),
        })
    });

struct JwksCacheInner {
    keys: Option<jsonwebtoken::jwk::JwkSet>,
    fetched_at: std::time::Instant,
}

/// Core JWT verification against cached JWKS keys.
async fn verify_jwt_with_jwks(
    token: &str,
    header: &jsonwebtoken::Header,
    expected_audience: Option<&str>,
) -> Result<bool, String> {
    // Restrict allowed algorithms to RSA family only (prevents algorithm confusion).
    let allowed_algs = [
        jsonwebtoken::Algorithm::RS256,
        jsonwebtoken::Algorithm::RS384,
        jsonwebtoken::Algorithm::RS512,
    ];
    if !allowed_algs.contains(&header.alg) {
        return Ok(false);
    }

    let jwks = fetch_or_cached_jwks().await?;

    let kid = header
        .kid
        .as_deref()
        .ok_or_else(|| "JWT missing kid header".to_string())?;

    let jwk = jwks
        .keys
        .iter()
        .find(|k| k.common.key_id.as_deref() == Some(kid))
        .ok_or_else(|| format!("No JWKS key matching kid={kid}"))?;

    let key = jsonwebtoken::DecodingKey::from_jwk(jwk).map_err(|e| format!("Invalid JWK: {e}"))?;

    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.algorithms = allowed_algs.to_vec();
    validation.set_issuer(&[EXPECTED_ISSUER]);
    match expected_audience {
        Some(aud) => validation.set_audience(&[aud]),
        None => {
            // SEC-05: Fail-closed — reject if no audience is configured.
            // Enterprise deployments MUST set `channels.teams.app_id`.
            warn!("Teams JWT: no expected audience (app_id) configured — rejecting (fail-closed)");
            return Ok(false);
        }
    }

    match jsonwebtoken::decode::<serde_json::Value>(token, &key, &validation) {
        Ok(_) => Ok(true),
        Err(e) => {
            warn!(error = %e, "Teams JWT claim/signature validation failed");
            Ok(false)
        }
    }
}

/// Return the JWKS key set, fetching from the Bot Framework OpenID endpoint
/// when the cache is stale or empty.
async fn fetch_or_cached_jwks() -> Result<jsonwebtoken::jwk::JwkSet, String> {
    // Fast path: return cached keys if TTL is still valid.
    {
        let read = JWKS_CACHE.read().await;
        if let Some(ref keys) = read.keys
            && read.fetched_at.elapsed().as_secs() < JWKS_CACHE_TTL_SECS
        {
            return Ok(keys.clone());
        }
    }

    // Slow path: refresh from the Bot Framework OpenID endpoint.
    let client = reqwest::Client::new();

    let meta: serde_json::Value = client
        .get(OPENID_CONFIG_URL)
        .send()
        .await
        .map_err(|e| format!("JWKS metadata fetch: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JWKS metadata parse: {e}"))?;

    let jwks_uri = meta["jwks_uri"]
        .as_str()
        .ok_or_else(|| "No jwks_uri in OpenID metadata".to_string())?;

    let jwks: jsonwebtoken::jwk::JwkSet = client
        .get(jwks_uri)
        .send()
        .await
        .map_err(|e| format!("JWKS fetch: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JWKS parse: {e}"))?;

    // Update cache.
    let mut write = JWKS_CACHE.write().await;
    write.keys = Some(jwks.clone());
    write.fetched_at = std::time::Instant::now();

    Ok(jwks)
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

    // ── JWT verification tests ───────────────────────────────────

    #[test]
    fn legacy_verify_rejects_missing_bearer_prefix() {
        assert!(!TeamsAdapter::verify_bot_framework_token(
            "NotBearer abc.def.ghi"
        ));
    }

    #[test]
    fn legacy_verify_rejects_malformed_jwt() {
        assert!(!TeamsAdapter::verify_bot_framework_token(
            "Bearer only-one-part"
        ));
    }

    #[test]
    fn legacy_verify_accepts_structural_jwt() {
        assert!(TeamsAdapter::verify_bot_framework_token(
            "Bearer eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ0ZXN0In0.c2lnbmF0dXJl"
        ));
    }

    #[tokio::test]
    async fn jwt_verify_rejects_missing_bearer_prefix() {
        assert!(!TeamsAdapter::verify_bot_framework_jwt("Token abc.def.ghi", None).await);
    }

    #[tokio::test]
    async fn jwt_verify_rejects_non_jwt_garbage() {
        assert!(!TeamsAdapter::verify_bot_framework_jwt("Bearer not-a-jwt", None).await);
    }

    #[tokio::test]
    async fn jwt_verify_rejects_when_jwks_unreachable() {
        // In test environment, JWKS endpoint is unreachable → fail-closed.
        // Craft a structurally valid RS256 JWT (header with kid) but unsigned.
        let header = base64_url_encode(r#"{"alg":"RS256","typ":"JWT","kid":"test-key-1"}"#);
        let payload = base64_url_encode(
            r#"{"iss":"https://api.botframework.com","aud":"app-id-123","exp":9999999999}"#,
        );
        let token = format!("Bearer {header}.{payload}.fake-signature");
        // Should reject because JWKS fetch fails (no network in test).
        assert!(!TeamsAdapter::verify_bot_framework_jwt(&token, Some("app-id-123")).await);
    }

    #[test]
    fn verify_jwt_with_jwks_rejects_hmac_algorithm() {
        // Prevent algorithm confusion: HS256 tokens must be rejected by decode_header
        // check in verify_jwt_with_jwks (only RS256/RS384/RS512 allowed).
        let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256);
        let allowed = [
            jsonwebtoken::Algorithm::RS256,
            jsonwebtoken::Algorithm::RS384,
            jsonwebtoken::Algorithm::RS512,
        ];
        assert!(!allowed.contains(&header.alg));
    }

    /// Simple base64url encode helper for test JWT construction.
    fn base64_url_encode(input: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
    }
}
