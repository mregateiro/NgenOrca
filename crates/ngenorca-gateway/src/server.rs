//! HTTP/WebSocket server startup.

use crate::auth;
use crate::rate_limit::RateLimiterState;
use crate::routes;
use crate::state::AppState;
use axum::middleware;
use ngenorca_core::{Error, Result};
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

/// Run the gateway server.
pub async fn run(state: AppState, bind: &str, port: u16) -> Result<()> {
    let auth_mode = format!("{:?}", state.config().gateway.auth_mode);
    let channels: Vec<String> = state.config().enabled_channels().into_iter().map(|s| s.to_owned()).collect();
    let (provider, model) = {
        let (p, m) = state.config().parse_model();
        (p.to_owned(), m.to_owned())
    };

    let limiter = RateLimiterState::with_metrics(
        state.config().gateway.rate_limit_max,
        state.config().gateway.rate_limit_window_secs,
        state.metrics().clone(),
    );
    let rate_limit_info = if state.config().gateway.rate_limit_max > 0 {
        format!(
            "{} req / {}s",
            state.config().gateway.rate_limit_max,
            state.config().gateway.rate_limit_window_secs,
        )
    } else {
        "disabled".into()
    };

    let cors_origins = &state.config().gateway.cors_allowed_origins;
    let cors_info: String;
    let cors_layer = if cors_origins.is_empty() {
        cors_info = "permissive (allow all)".into();
        CorsLayer::permissive()
    } else {
        cors_info = format!("{:?}", cors_origins);
        let origins: Vec<axum::http::HeaderValue> = cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods(tower_http::cors::Any)
            .allow_headers(tower_http::cors::Any)
    };

    let state_for_middleware = state.clone();
    let app = routes::router(state)
        .layer(middleware::from_fn_with_state(
            limiter,
            crate::rate_limit::rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state_for_middleware,
            auth::auth_middleware,
        ))
        .layer(middleware::from_fn(
            crate::request_id::request_id_middleware,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors_layer);

    // NOTE: The gateway binds plain TCP. TLS termination is delegated to a
    // reverse proxy (nginx, Caddy, Traefik, etc.). The tls_cert/tls_key fields
    // in config are used only for mTLS client-certificate auth validation at
    // the proxy layer, not by this server.
    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| Error::Gateway(format!("Failed to bind to {addr}: {e}")))?;

    info!("NgenOrca gateway listening on {}", addr);
    info!("  Auth:      {}", auth_mode);
    info!("  RateLimit: {}", rate_limit_info);
    info!("  CORS:      {}", cors_info);
    info!("  Provider:  {}/{}", provider, model);
    info!("  Channels:  {:?}", channels);
    info!("  Health:    http://{}/health", addr);
    info!("  Version:   http://{}/api/v1/version", addr);
    info!("  Status:    http://{}/api/v1/status", addr);
    info!("  Chat:      POST http://{}/api/v1/chat", addr);
    info!("  WebSocket: ws://{}/ws", addr);
    info!("  Whoami:    http://{}/api/v1/whoami", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| Error::Gateway(format!("Server error: {e}")))?;

    info!("Server stopped");
    Ok(())
}

/// Wait for a shutdown signal (Ctrl+C or SIGTERM on Unix).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => { info!("Received Ctrl+C, shutting down gracefully…"); },
        () = terminate => { info!("Received SIGTERM, shutting down gracefully…"); },
    }
}
