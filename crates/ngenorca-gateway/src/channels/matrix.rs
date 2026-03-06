//! Matrix adapter — stub implementation.
//!
//! Connects to any Matrix homeserver using the Client–Server API.
//! Supports optional end-to-end encryption and auto-join.
//! Registers in the plugin system for health checks and lifecycle.
//! Real message handling will be added in a future release.

use async_trait::async_trait;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::PluginId;
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext};
use tracing::info;

/// Matrix adapter.
pub struct MatrixAdapter {
    homeserver: String,
    user_id: String,
    access_token: String,
    device_id: Option<String>,
    auto_join: bool,
    encrypted: bool,
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
        }
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
            permissions: vec![Permission::ReadMessages, Permission::WriteMessages],
            api_version: API_VERSION,
            description: "Matrix homeserver adapter (CS API)".into(),
        }
    }

    async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
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

    async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // TODO: GET /_matrix/client/versions on homeserver to verify connectivity
        Ok(())
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        info!("Matrix adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for MatrixAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // TODO: Long-poll /sync endpoint for incoming events
        info!(
            homeserver = %self.homeserver,
            "Matrix adapter: listening not yet implemented"
        );
        Ok(())
    }

    async fn send_message(&self, _message: &Message) -> ngenorca_core::Result<()> {
        // TODO: PUT /_matrix/client/r0/rooms/{roomId}/send/m.room.message/{txnId}
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Matrix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_manifest() {
        let adapter = MatrixAdapter::new(
            "https://matrix.example.org".into(),
            "@bot:example.org".into(),
            "syt_token".into(),
            None,
            true,
            false,
        );
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Matrix");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[tokio::test]
    async fn matrix_health_check_ok() {
        let adapter = MatrixAdapter::new(
            "https://matrix.example.org".into(),
            "@bot:example.org".into(),
            "syt_token".into(),
            None,
            true,
            false,
        );
        assert!(adapter.health_check().await.is_ok());
    }

    #[test]
    fn matrix_channel_kind() {
        let adapter = MatrixAdapter::new(
            "https://matrix.example.org".into(),
            "@bot:example.org".into(),
            "syt_token".into(),
            None,
            true,
            false,
        );
        assert_eq!(adapter.channel_kind(), ChannelKind::Matrix);
    }
}
