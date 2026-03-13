//! WebChat adapter — a lightweight adapter that represents the built-in
//! WebSocket-based chat UI.
//!
//! The actual message handling happens in the WebSocket route handler.
//! This adapter exists so WebChat appears in the plugin list, health checks,
//! and can be managed through the same lifecycle as external adapters.

use async_trait::async_trait;
use ngenorca_core::ChannelKind;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{API_VERSION, Permission, PluginKind, PluginManifest};
use ngenorca_core::types::PluginId;
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext};

/// Built-in WebChat adapter.
pub struct WebChatAdapter {
    _initialized: bool,
}

impl Default for WebChatAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl WebChatAdapter {
    pub fn new() -> Self {
        Self {
            _initialized: false,
        }
    }
}

#[async_trait]
impl Plugin for WebChatAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-webchat".into()),
            name: "WebChat".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![Permission::ReadMessages, Permission::WriteMessages],
            api_version: API_VERSION,
            description: "Built-in WebSocket chat interface".into(),
        }
    }

    async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
        self._initialized = true;
        Ok(())
    }

    async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
        // WebChat messages are handled directly by the WebSocket route handler,
        // not through this adapter. This is a no-op.
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // Always healthy — the WebSocket server handles the real health.
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for WebChatAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // No-op — the WebSocket endpoint is started by the axum server.
        Ok(())
    }

    async fn send_message(&self, _message: &Message) -> ngenorca_core::Result<()> {
        // Outbound WebChat messages are pushed via WebSocket, not through this adapter.
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::WebChat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webchat_manifest() {
        let adapter = WebChatAdapter::new();
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "WebChat");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[tokio::test]
    async fn webchat_health_check_ok() {
        let adapter = WebChatAdapter::new();
        assert!(adapter.health_check().await.is_ok());
    }

    #[test]
    fn webchat_channel_kind() {
        let adapter = WebChatAdapter::new();
        assert_eq!(adapter.channel_kind(), ChannelKind::WebChat);
    }
}
