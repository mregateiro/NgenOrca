//! Microsoft Teams adapter — stub implementation.
//!
//! Uses the Bot Framework REST API with Azure AD authentication.
//! Registers in the plugin system for health checks and lifecycle.
//! Real message handling will be added in a future release.

use async_trait::async_trait;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::PluginId;
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext};
use tracing::info;

/// Microsoft Teams adapter via Bot Framework.
pub struct TeamsAdapter {
    app_id: String,
    app_password: String,
    tenant_id: Option<String>,
    webhook_url: Option<String>,
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
        }
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
            permissions: vec![Permission::ReadMessages, Permission::WriteMessages],
            api_version: API_VERSION,
            description: "Microsoft Teams adapter via Bot Framework".into(),
        }
    }

    async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
        info!(
            app_id = %self.app_id,
            has_tenant = self.tenant_id.is_some(),
            has_webhook = self.webhook_url.is_some(),
            "Teams adapter initialized"
        );
        Ok(())
    }

    async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // TODO: Verify Azure AD token exchange works
        Ok(())
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        info!("Teams adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for TeamsAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // TODO: Register /api/messages webhook endpoint for Bot Framework
        info!("Teams adapter: listening not yet implemented");
        Ok(())
    }

    async fn send_message(&self, _message: &Message) -> ngenorca_core::Result<()> {
        // TODO: POST activity to Bot Framework serviceUrl
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Teams
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_manifest() {
        let adapter = TeamsAdapter::new(
            "app-id-123".into(),
            "app-secret".into(),
            Some("tenant-456".into()),
            None,
        );
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Teams");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[tokio::test]
    async fn teams_health_check_ok() {
        let adapter = TeamsAdapter::new(
            "app-id-123".into(),
            "app-secret".into(),
            None,
            None,
        );
        assert!(adapter.health_check().await.is_ok());
    }

    #[test]
    fn teams_channel_kind() {
        let adapter = TeamsAdapter::new(
            "app-id-123".into(),
            "app-secret".into(),
            None,
            None,
        );
        assert_eq!(adapter.channel_kind(), ChannelKind::Teams);
    }
}
