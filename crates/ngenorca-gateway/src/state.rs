//! Shared application state accessible from all route handlers.

use crate::metrics::Metrics;
use crate::orchestration::LearnedRouter;
use crate::plugins::PluginRegistry;
use crate::providers::ProviderRegistry;
use crate::sessions::SessionManager;
use ngenorca_bus::EventBus;
use ngenorca_config::NgenOrcaConfig;
use ngenorca_identity::IdentityManager;
use ngenorca_memory::MemoryManager;
use std::sync::Arc;

/// Shared state for the gateway, accessible from all handlers.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

/// Parameters for constructing [`AppState`].
///
/// Groups all required dependencies into a single struct so that
/// `AppState::new` avoids the `clippy::too_many_arguments` warning.
pub struct AppStateParams {
    pub config: NgenOrcaConfig,
    pub config_file_path: std::path::PathBuf,
    pub event_bus: EventBus,
    pub identity: IdentityManager,
    pub memory: MemoryManager,
    pub providers: ProviderRegistry,
    pub sessions: SessionManager,
    pub plugins: PluginRegistry,
    pub learned_router: LearnedRouter,
    pub metrics: Metrics,
}

struct AppStateInner {
    pub config: NgenOrcaConfig,
    pub config_file_path: std::path::PathBuf,
    pub event_bus: EventBus,
    pub identity: IdentityManager,
    pub memory: MemoryManager,
    pub providers: ProviderRegistry,
    pub sessions: SessionManager,
    pub plugins: PluginRegistry,
    pub learned_router: LearnedRouter,
    pub metrics: Metrics,
    pub start_time: std::time::Instant,
}

impl AppState {
    pub fn new(params: AppStateParams) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                config: params.config,
                config_file_path: params.config_file_path,
                event_bus: params.event_bus,
                identity: params.identity,
                memory: params.memory,
                providers: params.providers,
                sessions: params.sessions,
                plugins: params.plugins,
                learned_router: params.learned_router,
                metrics: params.metrics,
                start_time: std::time::Instant::now(),
            }),
        }
    }

    pub fn config(&self) -> &NgenOrcaConfig {
        &self.inner.config
    }

    pub fn config_file_path(&self) -> &std::path::Path {
        &self.inner.config_file_path
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.inner.event_bus
    }

    pub fn identity(&self) -> &IdentityManager {
        &self.inner.identity
    }

    pub fn memory(&self) -> &MemoryManager {
        &self.inner.memory
    }

    pub fn providers(&self) -> &ProviderRegistry {
        &self.inner.providers
    }

    pub fn sessions(&self) -> &SessionManager {
        &self.inner.sessions
    }

    pub fn plugins(&self) -> &PluginRegistry {
        &self.inner.plugins
    }

    pub fn learned_router(&self) -> &LearnedRouter {
        &self.inner.learned_router
    }

    pub fn metrics(&self) -> &Metrics {
        &self.inner.metrics
    }

    pub fn uptime(&self) -> std::time::Duration {
        self.inner.start_time.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn make_state() -> AppState {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let config = NgenOrcaConfig::default();
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_dir =
            std::env::temp_dir().join(format!("ngenorca_test_{}_{unique}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let config_file_path = temp_dir.join("config.toml");
        let event_bus = EventBus::new(":memory:").await.unwrap();
        let identity = IdentityManager::new(":memory:").unwrap();
        let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
        let providers = ProviderRegistry::from_config(&config);
        let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
        let (plugin_tx, _plugin_rx) = tokio::sync::mpsc::unbounded_channel();
        let plugins = PluginRegistry::new(plugin_tx, std::path::PathBuf::from("/tmp/test_plugins"));
        let learned_router = LearnedRouter::new(":memory:").unwrap();
        let metrics = Metrics::new();
        AppState::new(AppStateParams {
            config,
            config_file_path,
            event_bus,
            identity,
            memory,
            providers,
            sessions,
            plugins,
            learned_router,
            metrics,
        })
    }

    #[tokio::test]
    async fn state_creation() {
        let state = make_state().await;
        assert_eq!(state.config().gateway.port, 18789);
    }

    #[tokio::test]
    async fn state_clone_shares_inner() {
        let state = make_state().await;
        let cloned = state.clone();
        assert_eq!(state.config().gateway.port, cloned.config().gateway.port);
    }

    #[tokio::test]
    async fn state_uptime_is_non_zero() {
        let state = make_state().await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(state.uptime().as_millis() >= 1);
    }

    #[tokio::test]
    async fn state_event_bus_works() {
        let state = make_state().await;
        assert_eq!(state.event_bus().event_count().unwrap(), 0);
    }
}
