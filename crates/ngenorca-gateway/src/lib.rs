//! # NgenOrca Gateway
//!
//! The central control plane. It:
//!
//! 1. Starts the HTTP + WebSocket server
//! 2. Initializes the event bus, identity, memory, and sandbox subsystems
//! 3. Loads and manages plugins (channel adapters, tools, model providers)
//! 4. Routes inbound messages → identity resolution → session → agent → response
//! 5. Serves the control UI and health endpoints

pub mod auth;
pub mod channels;
pub mod orchestration;
pub mod plugins;
pub mod providers;
pub mod routes;
pub mod server;
pub mod sessions;
pub mod state;

use ngenorca_bus::EventBus;
use ngenorca_config::NgenOrcaConfig;
use ngenorca_core::Result;
use ngenorca_identity::IdentityManager;
use ngenorca_memory::MemoryManager;
use plugins::PluginRegistry;
use providers::ProviderRegistry;
use sessions::SessionManager;
use tracing::info;

/// Start the NgenOrca gateway.
pub async fn start(config: NgenOrcaConfig) -> Result<()> {
    info!(
        bind = %config.gateway.bind,
        port = %config.gateway.port,
        model = %config.agent.model,
        "Starting NgenOrca gateway"
    );

    // Ensure data directory exists.
    std::fs::create_dir_all(&config.data_dir).ok();

    // Initialize subsystems.
    let db_path = config.data_dir.join("events.db");
    let event_bus = EventBus::new(db_path.to_str().unwrap_or("events.db")).await?;

    let identity_db = config.data_dir.join("identity.db");
    let identity = IdentityManager::new(identity_db.to_str().unwrap_or("identity.db"))?;

    let memory_dir = config.data_dir.join("memory");
    std::fs::create_dir_all(&memory_dir).ok();
    let memory = MemoryManager::new(memory_dir.to_str().unwrap_or("memory"))?;

    // Detect sandbox environment.
    let sandbox_env = ngenorca_sandbox::detect_environment();
    info!(?sandbox_env, "Sandbox environment detected");

    // Initialize model providers.
    let providers = ProviderRegistry::from_config(&config);
    info!(
        providers = ?providers.provider_names(),
        "Model providers registered"
    );

    // Initialize session manager.
    let sessions = SessionManager::new(
        config.agent.model.clone(),
        config.agent.thinking_level,
    );

    // Initialize plugin registry.
    // Create an mpsc channel that bridges plugin events → EventBus.
    let (plugin_tx, mut plugin_rx) = tokio::sync::mpsc::unbounded_channel();
    let plugin_dir = config.data_dir.join("plugins");
    std::fs::create_dir_all(&plugin_dir).ok();
    let plugin_registry = PluginRegistry::new(plugin_tx, plugin_dir);
    info!("Plugin registry initialized");

    // Spawn a bridge task: events emitted by plugins are forwarded to the bus.
    {
        let bus = event_bus.clone();
        tokio::spawn(async move {
            while let Some(event) = plugin_rx.recv().await {
                if let Err(e) = bus.publish(event).await {
                    tracing::warn!(error = %e, "Failed to forward plugin event to bus");
                }
            }
        });
    }

    // Initialize learned routing rules store.
    let learned_db_path = config.agent.workspace.join("learned_routes.db");
    let learned_router = orchestration::LearnedRouter::new(
        learned_db_path.to_str().unwrap_or("learned_routes.db"),
    )?;

    // Build shared application state.
    let app_state = state::AppState::new(
        config.clone(),
        event_bus,
        identity,
        memory,
        providers,
        sessions,
        plugin_registry,
        learned_router,
    );

    // Register channel adapters from config.
    channels::register_adapters(&config, app_state.plugins()).await?;
    info!(
        adapters = ?config.enabled_channels(),
        "Channel adapters registered"
    );

    // Spawn background memory consolidation task.
    {
        let state = app_state.clone();
        let consolidation_interval = std::time::Duration::from_secs(
            config.memory.consolidation_interval_secs,
        );
        let max_episodes = config.memory.episodic_max_entries;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(consolidation_interval);
            interval.tick().await; // Skip the immediate first tick
            loop {
                interval.tick().await;
                tracing::debug!("Running memory consolidation cycle");

                // Get distinct user IDs from episodic memory
                let users = state.memory().episodic.distinct_users()
                    .unwrap_or_default();

                for user_id in users {
                    let uid = ngenorca_core::types::UserId(user_id);
                    if let Err(e) = state.memory().consolidate_for_user(
                        &uid,
                        chrono::Duration::hours(24),
                        max_episodes,
                    ) {
                        tracing::warn!(
                            user = %uid,
                            error = %e,
                            "Memory consolidation failed for user"
                        );
                    }
                }
            }
        });
        info!(
            interval_secs = consolidation_interval.as_secs(),
            "Memory consolidation task started"
        );
    }

    // Start the HTTP/WebSocket server.
    server::run(app_state, &config.gateway.bind, config.gateway.port).await?;

    Ok(())
}
