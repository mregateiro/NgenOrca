//! HTTP/WebSocket server startup.

use crate::auth;
use crate::routes;
use crate::state::AppState;
use axum::middleware;
use ngenorca_core::{Error, Result};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
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

    let state_for_middleware = state.clone();
    let app = routes::router(state)
        .layer(middleware::from_fn_with_state(
            state_for_middleware,
            auth::auth_middleware,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive());

    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| Error::Gateway(format!("Failed to bind to {addr}: {e}")))?;

    info!("NgenOrca gateway listening on {}", addr);
    info!("  Auth:      {}", auth_mode);
    info!("  Provider:  {}/{}", provider, model);
    info!("  Channels:  {:?}", channels);
    info!("  Health:    http://{}/health", addr);
    info!("  Status:    http://{}/api/v1/status", addr);
    info!("  Chat:      POST http://{}/api/v1/chat", addr);
    info!("  WebSocket: ws://{}/ws", addr);
    info!("  Whoami:    http://{}/api/v1/whoami", addr);

    axum::serve(listener, app)
        .await
        .map_err(|e| Error::Gateway(format!("Server error: {e}")))?;

    Ok(())
}
