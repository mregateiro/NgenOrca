//! Signal adapter — stub implementation.
//!
//! Interfaces with signal-cli (JSON-RPC or daemon mode).
//! Registers in the plugin system for health checks and lifecycle.
//! Real message handling will be added in a future release.

use async_trait::async_trait;
use ngenorca_core::message::Message;
use ngenorca_core::plugin::{Permission, PluginKind, PluginManifest, API_VERSION};
use ngenorca_core::types::PluginId;
use ngenorca_core::ChannelKind;
use ngenorca_plugin_sdk::{ChannelAdapter, Plugin, PluginContext};
use tracing::info;

/// Signal adapter using signal-cli backend.
pub struct SignalAdapter {
    phone_number: String,
    signal_cli_path: String,
    data_path: String,
    mode: String,
}

impl SignalAdapter {
    pub fn new(
        phone_number: String,
        signal_cli_path: String,
        data_path: String,
        mode: String,
    ) -> Self {
        Self {
            phone_number,
            signal_cli_path,
            data_path,
            mode,
        }
    }
}

#[async_trait]
impl Plugin for SignalAdapter {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PluginId("ngenorca-signal".into()),
            name: "Signal".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: PluginKind::ChannelAdapter,
            permissions: vec![Permission::ReadMessages, Permission::WriteMessages],
            api_version: API_VERSION,
            description: "Signal adapter via signal-cli".into(),
        }
    }

    async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
        info!(
            phone = %self.phone_number,
            mode = %self.mode,
            cli_path = %self.signal_cli_path,
            "Signal adapter initialized"
        );
        Ok(())
    }

    async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
        Ok(None)
    }

    async fn health_check(&self) -> ngenorca_core::Result<()> {
        // TODO: Check that signal-cli process is reachable
        Ok(())
    }

    async fn shutdown(&self) -> ngenorca_core::Result<()> {
        info!("Signal adapter shutting down");
        Ok(())
    }
}

#[async_trait]
impl ChannelAdapter for SignalAdapter {
    async fn start_listening(&self) -> ngenorca_core::Result<()> {
        // TODO: Spawn signal-cli in JSON-RPC daemon mode, read incoming messages
        info!(
            mode = %self.mode,
            "Signal adapter: listening not yet implemented"
        );
        Ok(())
    }

    async fn send_message(&self, _message: &Message) -> ngenorca_core::Result<()> {
        // TODO: Call signal-cli send --json-rpc
        Ok(())
    }

    fn channel_kind(&self) -> ChannelKind {
        ChannelKind::Signal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_manifest() {
        let adapter = SignalAdapter::new(
            "+1234567890".into(),
            "/usr/bin/signal-cli".into(),
            "/var/lib/signal-cli".into(),
            "json-rpc".into(),
        );
        let manifest = adapter.manifest();
        assert_eq!(manifest.name, "Signal");
        assert_eq!(manifest.kind, PluginKind::ChannelAdapter);
    }

    #[tokio::test]
    async fn signal_health_check_ok() {
        let adapter = SignalAdapter::new(
            "+1234567890".into(),
            "/usr/bin/signal-cli".into(),
            "/var/lib/signal-cli".into(),
            "json-rpc".into(),
        );
        assert!(adapter.health_check().await.is_ok());
    }

    #[test]
    fn signal_channel_kind() {
        let adapter = SignalAdapter::new(
            "+1234567890".into(),
            "/usr/bin/signal-cli".into(),
            "/var/lib/signal-cli".into(),
            "json-rpc".into(),
        );
        assert_eq!(adapter.channel_kind(), ChannelKind::Signal);
    }
}
