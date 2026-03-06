//! Plugin SDK trait definitions.
//!
//! Every plugin (channel adapter, tool, integration) implements these traits.
//! The core gateway loads and manages plugins through this interface.

use serde::{Deserialize, Serialize};

use crate::types::PluginId;

/// Metadata about a plugin, declared at registration time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier.
    pub id: PluginId,

    /// Human-readable name.
    pub name: String,

    /// Version (semver).
    pub version: String,

    /// What kind of plugin this is.
    pub kind: PluginKind,

    /// Capabilities this plugin requires.
    pub permissions: Vec<Permission>,

    /// API version this plugin targets.
    pub api_version: u32,

    /// Human-readable description.
    pub description: String,
}

/// The kind of plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginKind {
    /// Channel adapter (WhatsApp, Telegram, etc.).
    ChannelAdapter,
    /// Tool that the agent can invoke.
    Tool,
    /// Memory backend or middleware.
    Memory,
    /// Model provider (Ollama, OpenAI, etc.).
    ModelProvider,
    /// Generic extension.
    Extension,
}

/// Permissions a plugin can request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// Read messages from the bus.
    ReadMessages,
    /// Write/send messages.
    WriteMessages,
    /// Access the network (HTTP, WebSocket).
    Network,
    /// Execute OS commands.
    Execute,
    /// Read files from the workspace.
    FileRead,
    /// Write files to the workspace.
    FileWrite,
    /// Access user identity information.
    Identity,
    /// Access memory tiers.
    Memory,
    /// Access configuration.
    Config,
}

/// Current NgenOrca plugin API version.
pub const API_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PluginId;

    #[test]
    fn api_version_is_one() {
        assert_eq!(API_VERSION, 1);
    }

    #[test]
    fn plugin_kind_serde_roundtrip() {
        let kinds = vec![
            PluginKind::ChannelAdapter,
            PluginKind::Tool,
            PluginKind::Memory,
            PluginKind::ModelProvider,
            PluginKind::Extension,
        ];
        for kind in &kinds {
            let json = serde_json::to_string(kind).unwrap();
            let back: PluginKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }

    #[test]
    fn permission_serde_roundtrip() {
        let perms = vec![
            Permission::ReadMessages,
            Permission::WriteMessages,
            Permission::Network,
            Permission::Execute,
            Permission::FileRead,
            Permission::FileWrite,
            Permission::Identity,
            Permission::Memory,
            Permission::Config,
        ];
        for perm in &perms {
            let json = serde_json::to_string(perm).unwrap();
            let back: Permission = serde_json::from_str(&json).unwrap();
            assert_eq!(*perm, back);
        }
    }

    #[test]
    fn plugin_manifest_serde_roundtrip() {
        let manifest = PluginManifest {
            id: PluginId("test-plugin".into()),
            name: "Test Plugin".into(),
            version: "0.1.0".into(),
            kind: PluginKind::Tool,
            permissions: vec![Permission::Network, Permission::FileRead],
            api_version: API_VERSION,
            description: "A test plugin".into(),
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.id, back.id);
        assert_eq!(manifest.name, back.name);
        assert_eq!(manifest.version, back.version);
        assert_eq!(manifest.kind, back.kind);
        assert_eq!(manifest.permissions.len(), 2);
        assert_eq!(manifest.api_version, back.api_version);
    }
}
