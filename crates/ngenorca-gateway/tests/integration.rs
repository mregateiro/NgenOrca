//! HTTP integration tests for the NgenOrca gateway.
//!
//! These tests build the full middleware stack (auth, rate-limit, request-ID)
//! and exercise routes via `tower::ServiceExt::oneshot`, without binding to a
//! real TCP port.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use ngenorca_bus::EventBus;
use ngenorca_config::NgenOrcaConfig;
use ngenorca_gateway::auth;
use ngenorca_gateway::metrics::Metrics;
use ngenorca_gateway::orchestration::LearnedRouter;
use ngenorca_gateway::plugins::PluginRegistry;
use ngenorca_gateway::providers::ProviderRegistry;
use ngenorca_gateway::rate_limit::RateLimiterState;
use ngenorca_gateway::routes;
use ngenorca_gateway::state::{AppState, AppStateParams};
use ngenorca_gateway::sessions::SessionManager;
use ngenorca_identity::IdentityManager;
use ngenorca_memory::MemoryManager;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use tower::ServiceExt;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build a test `AppState` backed by in-memory / temp-dir stores.
async fn test_state() -> AppState {
    let config = NgenOrcaConfig::default();
    let event_bus = EventBus::new(":memory:").await.unwrap();
    let identity = IdentityManager::new(":memory:").unwrap();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "ngenorca_integ_{}_{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(
        config.agent.model.clone(),
        config.agent.thinking_level,
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    AppState::new(AppStateParams {
        config,
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

/// Build the full middleware stack (matching `server::run`), returning a
/// `Router` suitable for `oneshot` calls.
fn build_app(state: AppState) -> axum::Router {
    let limiter = RateLimiterState::with_metrics(
        state.config().gateway.rate_limit_max,
        state.config().gateway.rate_limit_window_secs,
        state.metrics().clone(),
    );
    let state_for_auth = state.clone();
    routes::router(state)
        .layer(middleware::from_fn_with_state(
            limiter,
            ngenorca_gateway::rate_limit::rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state_for_auth,
            auth::auth_middleware,
        ))
        .layer(middleware::from_fn(
            ngenorca_gateway::request_id::request_id_middleware,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
}

// ─── Health endpoint ────────────────────────────────────────────────

#[tokio::test]
async fn health_returns_200_without_auth() {
    let app = build_app(test_state().await);
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "healthy");
}

// ─── Version endpoint ───────────────────────────────────────────────

#[tokio::test]
async fn version_returns_crate_version() {
    let app = build_app(test_state().await);
    // Version endpoint requires auth — default config is AuthMode::None so it passes.
    let req = Request::builder()
        .uri("/api/v1/version")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert!(json["version"].is_string());
    // Must not be empty.
    assert!(!json["version"].as_str().unwrap().is_empty());
}

// ─── Root endpoint ──────────────────────────────────────────────────

#[tokio::test]
async fn root_returns_name_and_endpoints() {
    let app = build_app(test_state().await);
    let req = Request::builder()
        .uri("/")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "NgenOrca");
    assert!(json["endpoints"].is_object());
}

// ─── Request-ID propagation ─────────────────────────────────────────

#[tokio::test]
async fn request_id_generated_when_absent() {
    let app = build_app(test_state().await);
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert!(resp.headers().contains_key("x-request-id"));
    let id = resp.headers().get("x-request-id").unwrap().to_str().unwrap();
    // UUIDv7 is 36 chars.
    assert_eq!(id.len(), 36);
}

#[tokio::test]
async fn request_id_propagated_from_client() {
    let app = build_app(test_state().await);
    let custom_id = "my-custom-request-id-12345";
    let req = Request::builder()
        .uri("/health")
        .header("x-request-id", custom_id)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let echo = resp.headers().get("x-request-id").unwrap().to_str().unwrap();
    assert_eq!(echo, custom_id);
}

// ─── Auth middleware — Token mode ────────────────────────────────────

#[tokio::test]
async fn token_auth_rejects_without_bearer() {
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::Token;
    config.gateway.auth_tokens = vec!["secret-token".into()];

    // We need to build state with this custom config.
    let event_bus = EventBus::new(":memory:").await.unwrap();
    let identity = IdentityManager::new(":memory:").unwrap();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "ngenorca_integ_token_{}_{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(
        config.agent.model.clone(),
        config.agent.thinking_level,
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    let state = AppState::new(AppStateParams {
        config,
        event_bus,
        identity,
        memory,
        providers,
        sessions,
        plugins,
        learned_router,
        metrics,
    });

    let app = build_app(state);

    // Non-exempt endpoint without token → 401.
    let req = Request::builder()
        .uri("/api/v1/status")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_auth_passes_with_valid_bearer() {
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::Token;
    config.gateway.auth_tokens = vec!["secret-token".into()];

    let event_bus = EventBus::new(":memory:").await.unwrap();
    let identity = IdentityManager::new(":memory:").unwrap();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "ngenorca_integ_token_ok_{}_{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(
        config.agent.model.clone(),
        config.agent.thinking_level,
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    let state = AppState::new(AppStateParams {
        config,
        event_bus,
        identity,
        memory,
        providers,
        sessions,
        plugins,
        learned_router,
        metrics,
    });

    let app = build_app(state);

    let req = Request::builder()
        .uri("/api/v1/status")
        .header("Authorization", "Bearer secret-token")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ─── Chat endpoint — message validation ──────────────────────────────

#[tokio::test]
async fn chat_rejects_empty_message() {
    let app = build_app(test_state().await);
    let body = serde_json::json!({ "message": "   " });
    let req = Request::builder()
        .uri("/api/v1/chat")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK); // returns 200 with error JSON

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["error"].as_str().unwrap().contains("empty"));
}

#[tokio::test]
async fn chat_rejects_oversized_message() {
    let mut config = NgenOrcaConfig::default();
    config.gateway.max_message_length = 50;

    let event_bus = EventBus::new(":memory:").await.unwrap();
    let identity = IdentityManager::new(":memory:").unwrap();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "ngenorca_integ_maxlen_{}_{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(
        config.agent.model.clone(),
        config.agent.thinking_level,
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    let state = AppState::new(AppStateParams {
        config,
        event_bus,
        identity,
        memory,
        providers,
        sessions,
        plugins,
        learned_router,
        metrics,
    });

    let app = build_app(state);

    let big_msg = "x".repeat(100);
    let body = serde_json::json!({ "message": big_msg });
    let req = Request::builder()
        .uri("/api/v1/chat")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["error"].as_str().unwrap().contains("too long"));
}

// ─── 404 for unknown routes ──────────────────────────────────────────

#[tokio::test]
async fn unknown_route_returns_404() {
    let app = build_app(test_state().await);
    let req = Request::builder()
        .uri("/nonexistent-path")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ─── Metrics endpoint ────────────────────────────────────────────────

#[tokio::test]
async fn metrics_endpoint_returns_ok() {
    let app = build_app(test_state().await);
    let req = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // /metrics is exempt from auth (like /health).
    assert_eq!(resp.status(), StatusCode::OK);
}
