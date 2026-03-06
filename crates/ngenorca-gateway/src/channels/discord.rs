//! Discord Bot adapter — stub implementation.
//!
//! Registers in the plugin system so Discord appears in health checks and
//! the adapter list. Real message handling via the Discord Gateway API
//! (serenity / twilight) will be added in a future release.

use async_trait::async_trait;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::PluginId;
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext};
use tracing::info;

/// Discord Bot adapter.
pub struct DiscordAdapter {
    bot_token: String,
    guild_ids: Vec<String>,
    command_prefix: Option<String>,
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
        }
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
            permissions: vec![Permission::ReadMessages, Permission::WriteMessages],
            api_version: API_VERSION,
            description: "Discord Bot adapter via Gateway API".into(),
        }
    }

    async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
        info!(
            guilds = ?self.guild_ids,
            prefix = ?self.command_prefix,
            "Discord adapter initialized (token length: {})",
            self.bot_token.len()
        );
        Ok(())
    }

    async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
        // Inbound messages will be received via the Discord Gateway connection
        // in start_listening(). This handler processes outbound responses.
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // TODO: Verify bot token with Discord API /users/@me
        Ok(())
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        info!("Discord adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for DiscordAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // TODO: Connect to Discord Gateway WebSocket, handle READY, MESSAGE_CREATE events,
        // convert to NgenOrca Messages, and push to the event bus.
        info!("Discord adapter: listening not yet implemented");
        Ok(())
    }

    async fn send_message(&self, _message: &Message) -> ngenorca_core::Result<()> {
        // TODO: POST to Discord channel via REST API.
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Discord
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_manifest() {
        let adapter = DiscordAdapter::new("test-token".into(), vec![], None);
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Discord");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[tokio::test]
    async fn discord_health_check_ok() {
        let adapter = DiscordAdapter::new("test-token".into(), vec![], None);
        assert!(adapter.health_check().await.is_ok());
    }

    #[test]
    fn discord_channel_kind() {
        let adapter = DiscordAdapter::new("test-token".into(), vec![], None);
        assert_eq!(adapter.channel_kind(), ChannelKind::Discord);
    }
}
