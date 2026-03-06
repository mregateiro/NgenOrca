//! WhatsApp Cloud API adapter — stub implementation.
//!
//! Uses Meta's WhatsApp Cloud API with webhook verification.
//! Registers in the plugin system for health checks and lifecycle.
//! Real message handling will be added in a future release.

use async_trait::async_trait;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::PluginId;
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext};
use tracing::info;

/// WhatsApp Cloud API adapter.
pub struct WhatsAppAdapter {
    phone_number_id: String,
    access_token: String,
    verify_token: String,
    webhook_path: String,
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
        }
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
            permissions: vec![Permission::ReadMessages, Permission::WriteMessages],
            api_version: API_VERSION,
            description: "WhatsApp Cloud API adapter".into(),
        }
    }

    async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
        info!(
            phone_number_id = %self.phone_number_id,
            webhook_path = %self.webhook_path,
            "WhatsApp adapter initialized (token length: {})",
            self.access_token.len()
        );
        Ok(())
    }

    async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // TODO: Validate token against WhatsApp Business API
        Ok(())
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        info!("WhatsApp adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for WhatsAppAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // TODO: Register webhook endpoint at self.webhook_path for incoming messages
        info!(
            path = %self.webhook_path,
            "WhatsApp adapter: listening not yet implemented"
        );
        Ok(())
    }

    async fn send_message(&self, _message: &Message) -> ngenorca_core::Result<()> {
        // TODO: POST to graph.facebook.com/v18.0/{phone_number_id}/messages
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::WhatsApp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_manifest() {
        let adapter = WhatsAppAdapter::new(
            "123456".into(),
            "EAAx...".into(),
            "my_verify".into(),
            "/webhook/whatsapp".into(),
        );
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "WhatsApp");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[tokio::test]
    async fn whatsapp_health_check_ok() {
        let adapter = WhatsAppAdapter::new(
            "123456".into(),
            "EAAx...".into(),
            "my_verify".into(),
            "/webhook/whatsapp".into(),
        );
        assert!(adapter.health_check().await.is_ok());
    }

    #[test]
    fn whatsapp_channel_kind() {
        let adapter = WhatsAppAdapter::new(
            "123456".into(),
            "EAAx...".into(),
            "my_verify".into(),
            "/webhook/whatsapp".into(),
        );
        assert_eq!(adapter.channel_kind(), ChannelKind::WhatsApp);
    }
}
