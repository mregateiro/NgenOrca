//! Slack Bot adapter — stub implementation.
//!
//! Supports Socket Mode (no public URL) or webhook mode.
//! Registers in the plugin system for health checks and lifecycle.
//! Real message handling will be added in a future release.

use async_trait::async_trait;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::PluginId;
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext};
use tracing::info;

/// Slack Bot adapter.
pub struct SlackAdapter {
    bot_token: String,
    app_token: Option<String>,
    socket_mode: bool,
}

impl SlackAdapter {
    pub fn new(bot_token: String, app_token: Option<String>, socket_mode: bool) -> Self {
        Self {
            bot_token,
            app_token,
            socket_mode,
        }
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
            permissions: vec![Permission::ReadMessages, Permission::WriteMessages],
            api_version: API_VERSION,
            description: "Slack Bot adapter (Socket Mode or Webhook)".into(),
        }
    }

    async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
        info!(
            socket_mode = self.socket_mode,
            has_app_token = self.app_token.is_some(),
            "Slack adapter initialized (bot token length: {})",
            self.bot_token.len()
        );
        Ok(())
    }

    async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // TODO: Call Slack auth.test to verify token
        Ok(())
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        info!("Slack adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for SlackAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // TODO: If socket_mode, open WebSocket to Slack. Otherwise, register webhook routes.
        info!("Slack adapter: listening not yet implemented");
        Ok(())
    }

    async fn send_message(&self, _message: &Message) -> ngenorca_core::Result<()> {
        // TODO: POST to Slack chat.postMessage API.
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Slack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_manifest() {
        let adapter = SlackAdapter::new("xoxb-test".into(), None, true);
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Slack");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[tokio::test]
    async fn slack_health_check_ok() {
        let adapter = SlackAdapter::new("xoxb-test".into(), None, true);
        assert!(adapter.health_check().await.is_ok());
    }

    #[test]
    fn slack_channel_kind() {
        let adapter = SlackAdapter::new("xoxb-test".into(), None, true);
        assert_eq!(adapter.channel_kind(), ChannelKind::Slack);
    }
}
