//! Plugin runtime — loads, manages lifecycle, and routes events to plugins.
//!
//! The gateway supports three plugin types at runtime:
//! - **ChannelAdapter**: bridges external messaging platforms
//! - **AgentTool**: gives the agent a capability (browser, file, exec, etc.)
//! - **Extension**: arbitrary hook into the lifecycle
//!
//! Model providers are handled separately via `ProviderRegistry`.

use ngenorca_core::event::Event;
use ngenorca_core::message::Message;
use ngenorca_core::types::PluginId;
use ngenorca_core::{Error, Result};
use ngenorca_plugin_sdk::{
    flume_like, AgentTool, Plugin, PluginContext, ToolDefinition,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Metadata about a loaded plugin.
#[derive(Debug, Clone)]
pub struct PluginEntry {
    /// Plugin name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// Whether the plugin is currently healthy.
    pub healthy: bool,
    /// Plugin kind (channel_adapter, tool, extension).
    pub kind: String,
}

/// Runtime registry that manages plugin lifecycle.
pub struct PluginRegistry {
    /// General plugins (extensions and channel adapters).
    plugins: RwLock<Vec<RegisteredPlugin>>,
    /// Agent tools indexed by name for fast lookup.
    tools: RwLock<HashMap<String, Arc<dyn AgentTool>>>,
    /// Channel adapters indexed by channel kind.
    adapters: RwLock<HashMap<String, usize>>, // index into plugins vec
    /// Event sender for plugins to emit events back to the bus.
    event_tx: tokio::sync::mpsc::UnboundedSender<Event>,
    /// Data directory root for plugin storage.
    data_dir: std::path::PathBuf,
}

struct RegisteredPlugin {
    #[allow(dead_code)]
    id: PluginId,
    plugin: Box<dyn Plugin>,
    healthy: bool,
}

impl PluginRegistry {
    /// Create a new plugin registry.
    ///
    /// The `event_tx` channel allows plugins to send events back into the gateway's
    /// event bus. The `data_dir` is the root directory under which each plugin gets
    /// its own subdirectory for persistent storage.
    pub fn new(
        event_tx: tokio::sync::mpsc::UnboundedSender<Event>,
        data_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            plugins: RwLock::new(Vec::new()),
            tools: RwLock::new(HashMap::new()),
            adapters: RwLock::new(HashMap::new()),
            event_tx,
            data_dir,
        }
    }

    /// Register and initialize a plugin.
    ///
    /// The plugin goes through the full lifecycle:
    /// 1. `manifest()` — read metadata
    /// 2. `init()` — provide context (sender, config, data_dir)
    /// 3. If it's a `ChannelAdapter`, register for routing
    pub async fn register(&self, mut plugin: Box<dyn Plugin>, config: serde_json::Value) -> Result<()> {
        let manifest = plugin.manifest();
        let id = manifest.id.0.clone();
        let name = manifest.name.clone();
        let version = manifest.version.clone();

        info!(
            id = %id,
            name = %name,
            version = %version,
            kind = ?manifest.kind,
            "Loading plugin"
        );

        // Create plugin-specific data directory (keyed on id for uniqueness)
        let plugin_data_dir = self.data_dir.join(&id);
        std::fs::create_dir_all(&plugin_data_dir).ok();

        // Build context for the plugin
        let ctx = PluginContext {
            sender: flume_like::Sender::new(self.event_tx.clone()),
            config,
            data_dir: plugin_data_dir,
        };

        // Initialize
        plugin.init(ctx).await.map_err(|e| {
            error!(plugin = %name, error = %e, "Plugin init failed");
            e
        })?;

        info!(plugin = %name, "Plugin initialized");

        // Store the plugin
        let mut plugins = self.plugins.write().await;
        let index = plugins.len();
        plugins.push(RegisteredPlugin {
            id: PluginId(id.clone()),
            plugin,
            healthy: true,
        });

        // If it declares adapter roles, register the channel mapping
        match manifest.kind {
            ngenorca_core::plugin::PluginKind::ChannelAdapter => {
                let mut adapters = self.adapters.write().await;
                adapters.insert(id.clone(), index);
                info!(plugin = %name, "Registered as channel adapter");
            }
            _ => {}
        }

        Ok(())
    }

    /// Register an agent tool.
    ///
    /// Tools are registered separately from plugins because they're invoked
    /// by the orchestrator during tool-call resolution rather than through
    /// the event bus.
    pub async fn register_tool(&self, tool: Arc<dyn AgentTool>) {
        let def = tool.definition();
        info!(
            tool = %def.name,
            description = %def.description,
            requires_sandbox = def.requires_sandbox,
            "Registered agent tool"
        );
        self.tools.write().await.insert(def.name.clone(), tool);
    }

    /// Get all tool definitions (for passing to the LLM's tool list).
    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .await
            .values()
            .map(|t| t.definition())
            .collect()
    }

    /// Execute a tool by name.
    pub async fn execute_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
        session_id: &ngenorca_core::SessionId,
        user_id: Option<&ngenorca_core::UserId>,
    ) -> Result<serde_json::Value> {
        let tools = self.tools.read().await;
        let tool = tools
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("Tool not found: {name}")))?;
        tool.execute(arguments, session_id, user_id).await
    }

    /// Route a message to the appropriate channel adapter.
    pub async fn route_to_adapter(&self, channel_kind: &str, message: &Message) -> Result<()> {
        let adapters = self.adapters.read().await;
        let index = adapters
            .get(channel_kind)
            .ok_or_else(|| Error::NotFound(format!("No adapter for channel: {channel_kind}")))?;

        let plugins = self.plugins.read().await;
        if let Some(entry) = plugins.get(*index) {
            match entry.plugin.handle_message(message).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    error!(adapter = %channel_kind, error = %e, "Adapter message handling failed");
                    Err(e)
                }
            }
        } else {
            Err(Error::NotFound(format!("Adapter index out of bounds: {index}")))
        }
    }

    /// Run health checks on all plugins.
    pub async fn health_check_all(&self) -> Vec<PluginEntry> {
        let mut plugins = self.plugins.write().await;
        let mut results = Vec::new();

        for entry in plugins.iter_mut() {
            let manifest = entry.plugin.manifest();
            let healthy = match entry.plugin.health_check().await {
                Ok(()) => {
                    entry.healthy = true;
                    true
                }
                Err(e) => {
                    warn!(
                        plugin = %manifest.name,
                        error = %e,
                        "Plugin health check failed"
                    );
                    entry.healthy = false;
                    false
                }
            };

            results.push(PluginEntry {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                healthy,
                kind: format!("{:?}", manifest.kind),
            });
        }

        results
    }

    /// Gracefully shut down all plugins.
    pub async fn shutdown_all(&self) {
        let plugins = self.plugins.read().await;
        for entry in plugins.iter() {
            let manifest = entry.plugin.manifest();
            if let Err(e) = entry.plugin.shutdown().await {
                warn!(
                    plugin = %manifest.name,
                    error = %e,
                    "Plugin shutdown error"
                );
            } else {
                debug!(plugin = %manifest.name, "Plugin shut down");
            }
        }
    }

    /// List all loaded plugins.
    pub async fn list_plugins(&self) -> Vec<PluginEntry> {
        let plugins = self.plugins.read().await;
        plugins
            .iter()
            .map(|entry| {
                let manifest = entry.plugin.manifest();
                PluginEntry {
                    name: manifest.name.clone(),
                    version: manifest.version.clone(),
                    healthy: entry.healthy,
                    kind: format!("{:?}", manifest.kind),
                }
            })
            .collect()
    }

    /// Get the number of registered tools.
    pub async fn tool_count(&self) -> usize {
        self.tools.read().await.len()
    }

    /// Get the number of loaded plugins.
    pub async fn plugin_count(&self) -> usize {
        self.plugins.read().await.len()
    }

    /// Check if a specific tool is available.
    pub async fn has_tool(&self, name: &str) -> bool {
        self.tools.read().await.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ngenorca_core::plugin::{PluginKind, PluginManifest, API_VERSION};

    /// A test plugin for verification.
    struct TestPlugin {
        name: String,
        initialized: std::sync::atomic::AtomicBool,
    }

    impl TestPlugin {
        fn new(name: &str) -> Self {
            Self {
                name: name.into(),
                initialized: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    #[async_trait]
    impl Plugin for TestPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: PluginId(self.name.clone()),
                name: self.name.clone(),
                version: "0.1.0".into(),
                kind: PluginKind::Extension,
                permissions: vec![],
                api_version: API_VERSION,
                description: "Test plugin".into(),
            }
        }

        async fn init(&mut self, _ctx: PluginContext) -> ngenorca_core::Result<()> {
            self.initialized.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        async fn handle_message(&self, _message: &Message) -> ngenorca_core::Result<Option<Message>> {
            Ok(None)
        }
    }

    /// A test tool.
    struct TestTool;

    #[async_trait]
    impl AgentTool for TestTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "test_tool".into(),
                description: "A test tool".into(),
                parameters: serde_json::json!({"type": "object"}),
                requires_sandbox: false,
            }
        }

        async fn execute(
            &self,
            arguments: serde_json::Value,
            _session_id: &ngenorca_core::SessionId,
            _user_id: Option<&ngenorca_core::UserId>,
        ) -> ngenorca_core::Result<serde_json::Value> {
            Ok(serde_json::json!({ "result": "ok", "input": arguments }))
        }
    }

    fn make_registry() -> PluginRegistry {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/ngenorca/plugins"))
    }

    #[tokio::test]
    async fn register_and_list_plugin() {
        let registry = make_registry();
        let plugin = Box::new(TestPlugin::new("test-ext"));

        registry
            .register(plugin, serde_json::json!({}))
            .await
            .unwrap();

        let plugins = registry.list_plugins().await;
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "test-ext");
        assert_eq!(plugins[0].version, "0.1.0");
        assert!(plugins[0].healthy);
    }

    #[tokio::test]
    async fn register_and_execute_tool() {
        let registry = make_registry();
        registry.register_tool(Arc::new(TestTool)).await;

        assert!(registry.has_tool("test_tool").await);
        assert_eq!(registry.tool_count().await, 1);

        let result = registry
            .execute_tool(
                "test_tool",
                serde_json::json!({"x": 1}),
                &ngenorca_core::SessionId::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(result["result"], "ok");
    }

    #[tokio::test]
    async fn tool_definitions_returns_all() {
        let registry = make_registry();
        registry.register_tool(Arc::new(TestTool)).await;

        let defs = registry.tool_definitions().await;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "test_tool");
    }

    #[tokio::test]
    async fn unknown_tool_returns_not_found() {
        let registry = make_registry();

        let result = registry
            .execute_tool(
                "nonexistent",
                serde_json::json!({}),
                &ngenorca_core::SessionId::new(),
                None,
            )
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn health_check_all_reports_healthy() {
        let registry = make_registry();
        let plugin = Box::new(TestPlugin::new("healthy-plugin"));
        registry
            .register(plugin, serde_json::json!({}))
            .await
            .unwrap();

        let results = registry.health_check_all().await;
        assert_eq!(results.len(), 1);
        assert!(results[0].healthy);
    }

    #[tokio::test]
    async fn shutdown_all_completes() {
        let registry = make_registry();
        let plugin = Box::new(TestPlugin::new("shutdown-test"));
        registry
            .register(plugin, serde_json::json!({}))
            .await
            .unwrap();

        // Should not panic
        registry.shutdown_all().await;
    }

    #[tokio::test]
    async fn plugin_count_tracks_registrations() {
        let registry = make_registry();
        assert_eq!(registry.plugin_count().await, 0);

        registry
            .register(
                Box::new(TestPlugin::new("p1")),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert_eq!(registry.plugin_count().await, 1);

        registry
            .register(
                Box::new(TestPlugin::new("p2")),
                serde_json::json!({}),
            )
            .await
            .unwrap();
        assert_eq!(registry.plugin_count().await, 2);
    }
}
