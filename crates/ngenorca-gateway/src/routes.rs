//! HTTP routes and WebSocket handler.

use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Json, IntoResponse},
    routing::{get, post},
    Extension,
    Router,
};
use futures::{SinkExt, StreamExt};
use ngenorca_core::types::UserId;
use ngenorca_plugin_sdk::ChatMessage;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::auth::CallerIdentity;
use crate::orchestration::HybridOrchestrator;
use crate::state::AppState;

/// Build the main router with all routes.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/api/v1/status", get(status))
        .route("/api/v1/whoami", get(whoami))
        .route("/api/v1/providers", get(providers))
        .route("/api/v1/channels", get(channels))
        .route("/api/v1/orchestration", get(orchestration_info))
        .route("/api/v1/orchestration/classify", axum::routing::post(classify_preview))
        .route("/api/v1/identity/users", get(list_users))
        .route("/api/v1/memory/stats", get(memory_stats))
        .route("/api/v1/events/count", get(event_count))
        // ── New endpoints ──
        .route("/api/v1/chat", post(chat))
        .route("/api/v1/sessions", get(list_sessions))
        .route("/ws", get(ws_handler))
        .route("/metrics", get(metrics_endpoint))
        .with_state(state)
}

/// Root endpoint.
async fn root() -> Json<serde_json::Value> {
    Json(json!({
        "name": "NgenOrca",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Personal AI Assistant — microkernel, hardware-bound identity, three-tier memory",
        "endpoints": {
            "health": "/health",
            "status": "/api/v1/status",
            "whoami": "/api/v1/whoami",
            "chat": "POST /api/v1/chat",
            "sessions": "/api/v1/sessions",
            "websocket": "WS /ws",
            "providers": "/api/v1/providers",
            "channels": "/api/v1/channels",
            "orchestration": "/api/v1/orchestration",
            "classify": "POST /api/v1/orchestration/classify",
            "users": "/api/v1/identity/users",
            "memory": "/api/v1/memory/stats",
            "events": "/api/v1/events/count",
        },
    }))
}

/// Health check endpoint (no auth required — used by nginx/Docker healthcheck).
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let uptime = state.uptime();
    let sandbox_env = ngenorca_sandbox::detect_environment();

    Json(json!({
        "status": "healthy",
        "uptime_secs": uptime.as_secs(),
        "sandbox_environment": format!("{:?}", sandbox_env),
    }))
}

/// Who am I? Shows the caller's identity as resolved by the auth middleware.
/// Useful for verifying Authelia → nginx → NgenOrca identity flow.
async fn whoami(
    Extension(caller): Extension<CallerIdentity>,
) -> Json<serde_json::Value> {
    Json(json!({
        "username": caller.username,
        "email": caller.email,
        "groups": caller.groups,
        "auth_method": format!("{:?}", caller.auth_method),
    }))
}

/// Status endpoint with system overview.
async fn status(
    State(state): State<AppState>,
    Extension(caller): Extension<CallerIdentity>,
) -> Json<serde_json::Value> {
    let uptime = state.uptime();
    let event_count = state.event_bus().event_count().unwrap_or(0);
    let sandbox_env = ngenorca_sandbox::detect_environment();
    let (provider, model) = state.config().parse_model();
    let channels = state.config().enabled_channels();

    Json(json!({
        "gateway": {
            "bind": state.config().gateway.bind,
            "port": state.config().gateway.port,
            "auth_mode": format!("{:?}", state.config().gateway.auth_mode),
            "uptime_secs": uptime.as_secs(),
        },
        "caller": {
            "username": caller.username,
            "auth_method": format!("{:?}", caller.auth_method),
        },
        "agent": {
            "provider": provider,
            "model": model,
            "thinking_level": format!("{:?}", state.config().agent.thinking_level),
        },
        "channels": {
            "enabled": channels,
        },
        "events": {
            "total": event_count,
        },
        "memory": {
            "enabled": state.config().memory.enabled,
        },
        "sandbox": {
            "enabled": state.config().sandbox.enabled,
            "environment": format!("{:?}", sandbox_env),
        },
    }))
}

/// Show configured LLM providers and which is active.
async fn providers(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let (active_provider, active_model) = state.config().parse_model();
    let providers = &state.config().agent.providers;

    Json(json!({
        "active": {
            "provider": active_provider,
            "model": active_model,
        },
        "configured": {
            "anthropic": providers.anthropic.as_ref().map(|p| json!({
                "base_url": p.base_url,
                "has_api_key": p.api_key.is_some(),
                "max_tokens": p.max_tokens,
                "temperature": p.temperature,
            })),
            "openai": providers.openai.as_ref().map(|p| json!({
                "base_url": p.base_url,
                "has_api_key": p.api_key.is_some(),
                "organization": p.organization,
            })),
            "ollama": providers.ollama.as_ref().map(|p| json!({
                "base_url": p.base_url,
                "keep_alive": p.keep_alive,
                "num_ctx": p.num_ctx,
            })),
            "azure": providers.azure.as_ref().map(|p| json!({
                "endpoint": p.endpoint,
                "has_api_key": p.api_key.is_some(),
                "deployment": p.deployment,
                "api_version": p.api_version,
            })),
            "google": providers.google.as_ref().map(|p| json!({
                "base_url": p.base_url,
                "has_api_key": p.api_key.is_some(),
            })),
            "openrouter": providers.openrouter.as_ref().map(|p| json!({
                "base_url": p.base_url,
                "has_api_key": p.api_key.is_some(),
                "site_name": p.site_name,
                "fallback_models": p.fallback_models,
            })),
            "custom": providers.custom.as_ref().map(|p| json!({
                "base_url": p.base_url,
                "has_api_key": p.api_key.is_some(),
                "model_name": p.model_name,
            })),
        },
    }))
}

/// Show configured channels and their status.
async fn channels(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let ch = &state.config().channels;

    Json(json!({
        "webchat": ch.webchat.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "theme": c.theme,
        })),
        "telegram": ch.telegram.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "polling": c.polling,
            "has_bot_token": c.bot_token.is_some(),
            "allowed_users": c.allowed_users.len(),
        })),
        "discord": ch.discord.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "has_bot_token": c.bot_token.is_some(),
            "guild_ids": c.guild_ids,
        })),
        "whatsapp": ch.whatsapp.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "has_access_token": c.access_token.is_some(),
            "webhook_path": c.webhook_path,
        })),
        "slack": ch.slack.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "socket_mode": c.socket_mode,
            "has_bot_token": c.bot_token.is_some(),
        })),
        "signal": ch.signal.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "mode": c.mode,
        })),
        "matrix": ch.matrix.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "homeserver": c.homeserver,
            "encrypted": c.encrypted,
        })),
        "teams": ch.teams.as_ref().map(|c| json!({
            "enabled": c.enabled,
            "tenant_id": c.tenant_id,
        })),
    }))
}

/// List all registered users.
async fn list_users(State(state): State<AppState>) -> Json<serde_json::Value> {
    match state.identity().list_users() {
        Ok(users) => {
            let user_list: Vec<serde_json::Value> = users
                .iter()
                .map(|u| {
                    json!({
                        "user_id": u.user_id.0,
                        "display_name": u.display_name,
                        "role": u.role,
                        "devices": u.devices.len(),
                        "channels": u.channels.len(),
                        "created_at": u.created_at.to_rfc3339(),
                        "last_seen": u.last_seen.to_rfc3339(),
                    })
                })
                .collect();
            Json(json!({ "users": user_list }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// Memory statistics.
async fn memory_stats(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "tier1_working": {
            "description": "Active conversation context windows (in-memory)",
        },
        "tier2_episodic": {
            "description": "Conversation history with semantic search (SQLite)",
        },
        "tier3_semantic": {
            "description": "Distilled facts and user knowledge (SQLite)",
        },
        "config": {
            "episodic_max_entries": state.config().memory.episodic_max_entries,
            "consolidation_interval_secs": state.config().memory.consolidation_interval_secs,
            "semantic_token_budget": state.config().memory.semantic_token_budget,
        },
    }))
}

/// Event count.
async fn event_count(State(state): State<AppState>) -> Json<serde_json::Value> {
    let count = state.event_bus().event_count().unwrap_or(0);
    Json(json!({ "total_events": count }))
}

/// Show orchestration configuration and sub-agents.
async fn orchestration_info(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let orch = HybridOrchestrator::new(std::sync::Arc::new(state.config().clone()));
    let info = orch.info();
    Json(serde_json::to_value(info).unwrap_or_else(|_| json!({ "error": "serialization failed" })))
}

/// Preview task classification for a message (does not execute — just classifies).
async fn classify_preview(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if message.is_empty() {
        return Json(json!({ "error": "provide a 'message' field" }));
    }

    let orch = HybridOrchestrator::new(std::sync::Arc::new(state.config().clone()));

    match orch.classify(message, Some(state.providers())).await {
        Ok(classification) => {
            let routing = orch.route(&classification);
            Json(json!({
                "classification": {
                    "intent": format!("{:?}", classification.intent),
                    "complexity": format!("{:?}", classification.complexity),
                    "confidence": classification.confidence,
                    "method": format!("{:?}", classification.method),
                    "language": classification.language,
                    "domain_tags": classification.domain_tags,
                },
                "routing": {
                    "target_agent": routing.target.name,
                    "target_model": routing.target.model,
                    "reason": routing.reason,
                    "temperature": routing.temperature,
                    "max_tokens": routing.max_tokens,
                    "from_memory": routing.from_memory,
                },
            }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

// ─── Chat Request/Response types ────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatRequest {
    /// The user's message.
    message: String,
    /// Optional session ID to continue a conversation.
    session_id: Option<String>,
    /// Optional conversation history (if not using sessions).
    #[serde(default)]
    conversation: Vec<ChatMessage>,
    /// Optional channel identifier.
    #[serde(default = "default_channel")]
    channel: String,
}

fn default_channel() -> String {
    "webchat".into()
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    /// The assistant's response.
    content: String,
    /// Session ID for future messages.
    session_id: String,
    /// Which model served this response.
    served_by: String,
    /// Classification info.
    classification: ClassificationInfo,
    /// Token usage.
    usage: UsageInfo,
    /// Latency in milliseconds.
    latency_ms: u64,
    /// Whether escalation occurred.
    escalated: bool,
}

#[derive(Debug, Serialize)]
struct ClassificationInfo {
    intent: String,
    complexity: String,
    confidence: f64,
    language: Option<String>,
}

#[derive(Debug, Serialize)]
struct UsageInfo {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

/// POST /api/v1/chat — main chat endpoint.
///
/// Sends a message through the full orchestration pipeline:
/// classify → route → memory context → delegate to LLM → quality gate → respond.
async fn chat(
    State(state): State<AppState>,
    Extension(caller): Extension<CallerIdentity>,
    Json(body): Json<ChatRequest>,
) -> Json<serde_json::Value> {
    state.metrics().inc_http_requests();

    if body.message.trim().is_empty() {
        return Json(json!({ "error": "message cannot be empty" }));
    }

    let user_id = caller.username.as_ref()
        .filter(|u| !u.is_empty())
        .map(|u| UserId(u.clone()));

    // ── Session ──
    let session_id = if let Some(ref sid_str) = body.session_id {
        match uuid::Uuid::parse_str(sid_str) {
            Ok(uuid) => {
                let sid = ngenorca_core::SessionId(uuid);
                // Verify session exists, otherwise create new
                if state.sessions().get(&sid).is_some() {
                    sid
                } else {
                    match state.sessions().get_or_create(user_id.as_ref(), &body.channel) {
                        Ok(sid) => sid,
                        Err(e) => return Json(json!({ "error": e.to_string() })),
                    }
                }
            }
            Err(_) => {
                match state.sessions().get_or_create(user_id.as_ref(), &body.channel) {
                    Ok(sid) => sid,
                    Err(e) => return Json(json!({ "error": e.to_string() })),
                }
            }
        }
    } else {
        match state.sessions().get_or_create(user_id.as_ref(), &body.channel) {
            Ok(sid) => sid,
            Err(e) => return Json(json!({ "error": e.to_string() })),
        }
    };

    // ── Build conversation context ──
    let mut conversation = body.conversation.clone();

    // Inject memory context if available
    if state.config().memory.enabled {
        if let Some(ref uid) = user_id {
            let token_budget = state.config().memory.semantic_token_budget;
            match state
                .memory()
                .build_context(uid, &session_id, &body.message, token_budget)
            {
                Ok(ctx) => {
                    // Prepend semantic facts as system context
                    if !ctx.semantic_block.is_empty() {
                        let facts: Vec<String> = ctx
                            .semantic_block
                            .iter()
                            .map(|f| format!("- {}", f.fact))
                            .collect();
                        conversation.insert(
                            0,
                            ChatMessage {
                                role: "system".into(),
                                content: format!(
                                    "Known facts about the user:\n{}",
                                    facts.join("\n")
                                ),
                            },
                        );
                    }

                    // Add episodic snippets as context
                    if !ctx.episodic_snippets.is_empty() {
                        let snippets: Vec<String> = ctx
                            .episodic_snippets
                            .iter()
                            .map(|e| e.content.clone())
                            .collect();
                        conversation.insert(
                            0,
                            ChatMessage {
                                role: "system".into(),
                                content: format!(
                                    "Relevant past conversations:\n{}",
                                    snippets.join("\n---\n")
                                ),
                            },
                        );
                    }

                    // Add working memory (current session messages)
                    for wm in &ctx.working_messages {
                        conversation.push(ChatMessage {
                            role: wm.role.clone(),
                            content: wm.content.clone(),
                        });
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to build memory context");
                }
            }
        }
    }

    // ── Store user message in working memory ──
    state.memory().working.push(
        &session_id,
        ngenorca_memory::working::WorkingMessage {
            role: "user".into(),
            content: body.message.clone(),
            timestamp: chrono::Utc::now(),
            estimated_tokens: body.message.len() / 4, // rough estimate
        },
    );

    // ── Orchestrate ──
    let orch = HybridOrchestrator::new(Arc::new(state.config().clone()));

    match orch
        .process(&body.message, &conversation, state.providers(), Some(state.plugins()), None)
        .await
    {
        Ok((response, record)) => {
            state.metrics().inc_orchestrations();
            state.metrics().add_tokens(response.total_usage.total_tokens as u64);
            if response.escalated {
                state.metrics().inc_escalations();
            }

            // Ingest orchestration record into learned routing rules
            if let Err(e) = state.learned_router().ingest(&record) {
                warn!(error = %e, "Failed to ingest learned routing rule");
            }

            // Publish orchestration record to event bus for analytics / learned routing
            let orch_event = ngenorca_core::event::Event {
                id: ngenorca_core::types::EventId::new(),
                timestamp: chrono::Utc::now(),
                session_id: Some(session_id.clone()),
                user_id: user_id.clone(),
                payload: ngenorca_core::event::EventPayload::OrchestrationCompleted(record),
            };
            if let Err(e) = state.event_bus().publish(orch_event).await {
                warn!(error = %e, "Failed to publish orchestration record");
            }

            // Store assistant response in working memory
            state.memory().working.push(
                &session_id,
                ngenorca_memory::working::WorkingMessage {
                    role: "assistant".into(),
                    content: response.content.clone(),
                    timestamp: chrono::Utc::now(),
                    estimated_tokens: response.content.len() / 4,
                },
            );

            // Update session
            let _ = state
                .sessions()
                .record_message(&session_id, response.total_usage.total_tokens);

            // Store in episodic memory
            if let Some(ref uid) = user_id {
                let entry = ngenorca_memory::episodic::EpisodicEntry {
                    id: 0,
                    user_id: uid.0.clone(),
                    content: format!("User: {}\nAssistant: {}", body.message, response.content),
                    summary: None,
                    channel: body.channel.clone(),
                    timestamp: chrono::Utc::now(),
                    embedding: None,
                    relevance_score: 0.0,
                };
                if let Err(e) = state.memory().episodic.store(&entry) {
                    warn!(error = %e, "Failed to store episodic memory");
                }
            }

            Json(json!(ChatResponse {
                content: response.content,
                session_id: session_id.to_string(),
                served_by: format!(
                    "{}/{}",
                    response.served_by.name, response.served_by.model
                ),
                classification: ClassificationInfo {
                    intent: format!("{:?}", response.classification.intent),
                    complexity: format!("{:?}", response.classification.complexity),
                    confidence: response.classification.confidence,
                    language: response.classification.language,
                },
                usage: UsageInfo {
                    prompt_tokens: response.total_usage.prompt_tokens,
                    completion_tokens: response.total_usage.completion_tokens,
                    total_tokens: response.total_usage.total_tokens,
                },
                latency_ms: response.latency_ms,
                escalated: response.escalated,
            }))
        }
        Err(e) => {
            state.metrics().inc_http_errors();
            error!(error = %e, "Chat orchestration failed");
            Json(json!({ "error": e.to_string() }))
        }
    }
}

/// GET /api/v1/sessions — list active sessions.
async fn list_sessions(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let sessions = state.sessions().active_sessions();
    let session_list: Vec<serde_json::Value> = sessions
        .iter()
        .map(|s| {
            json!({
                "id": s.id.to_string(),
                "user_id": s.user_id.as_ref().map(|u| &u.0),
                "channel": s.origin_channel.0,
                "state": format!("{:?}", s.state),
                "model": s.model,
                "message_count": s.message_count,
                "tokens_used": s.tokens_used,
                "created_at": s.created_at.to_rfc3339(),
                "last_active": s.last_active.to_rfc3339(),
            })
        })
        .collect();

    Json(json!({
        "active_sessions": session_list,
        "total": state.sessions().total_sessions(),
    }))
}

// ─── Metrics ────────────────────────────────────────────────────

/// GET /metrics — Prometheus-compatible metrics endpoint.
async fn metrics_endpoint(State(state): State<AppState>) -> String {
    state.metrics().render_prometheus()
}

// ─── WebSocket Handler ──────────────────────────────────────────

/// WebSocket message types for the client.
#[derive(Debug, Deserialize)]
struct WsChatMessage {
    /// The user's message.
    message: String,
    /// Optional session ID.
    #[serde(default)]
    #[allow(dead_code)]
    session_id: Option<String>,
}

/// WebSocket response types for the client.
#[derive(Debug, Serialize)]
struct WsChatResponse {
    #[serde(rename = "type")]
    msg_type: String,
    content: Option<String>,
    session_id: Option<String>,
    served_by: Option<String>,
    error: Option<String>,
    latency_ms: Option<u64>,
}

/// GET /ws — WebSocket upgrade handler.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Extension(caller): Extension<CallerIdentity>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, state, caller))
}

async fn handle_websocket(socket: WebSocket, state: AppState, caller: CallerIdentity) {
    state.metrics().inc_ws_connections();
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to real-time events from the bus for push notifications.
    let mut event_rx = state.event_bus().subscribe();

    let user_id = caller.username.as_ref()
        .filter(|u| !u.is_empty())
        .map(|u| UserId(u.clone()));

    let display_user = caller.username.as_deref().unwrap_or("anonymous");
    info!(
        user = %display_user,
        "WebSocket connection established"
    );

    // Send a welcome message
    let welcome = WsChatResponse {
        msg_type: "connected".into(),
        content: Some("Connected to NgenOrca".into()),
        session_id: None,
        served_by: None,
        error: None,
        latency_ms: None,
    };
    if let Ok(json) = serde_json::to_string(&welcome) {
        let _ = sender.send(WsMessage::Text(json.into())).await;
    }

    loop {
        tokio::select! {
            // Arm 1: Inbound messages from the WebSocket client
            client_msg = receiver.next() => {
                let msg = match client_msg {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(_))) => {
                        info!(user = %display_user, "WebSocket closed by client");
                        break;
                    }
                    Some(Ok(_)) => continue, // Ignore binary, ping, pong
                    Some(Err(e)) => {
                        warn!(error = %e, "WebSocket error");
                        break;
                    }
                    None => break, // Stream ended
                };

                state.metrics().inc_ws_messages_in();
                if let Err(done) = handle_client_message(
                    &msg, &state, &mut sender, user_id.as_ref(), display_user,
                ).await {
                    if done {
                        break;
                    }
                }
            }

            // Arm 2: Events pushed from the EventBus (broadcast to all WS clients)
            event = event_rx.recv() => {
                match event {
                    Ok(bus_event) => {
                        let ws_event = WsEventPush {
                            msg_type: "event".into(),
                            event_id: bus_event.id.to_string(),
                            event_kind: event_payload_kind(&bus_event.payload),
                            session_id: bus_event.session_id.as_ref().map(|s| s.to_string()),
                            payload: serde_json::to_value(&bus_event.payload).ok(),
                            timestamp: bus_event.timestamp.to_rfc3339(),
                        };
                        if let Ok(json) = serde_json::to_string(&ws_event) {
                            if sender.send(WsMessage::Text(json.into())).await.is_err() {
                                break; // Client disconnected
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(lagged = n, "WS event subscriber lagged, skipping events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("Event bus closed, ending WS push");
                        break;
                    }
                }
            }
        }
    }

    info!(user = %display_user, "WebSocket connection closed");
}

/// Process a single inbound client message. Returns `Err(true)` if the
/// connection should be closed, `Err(false)` if the message was handled
/// (but the caller should `continue`), and `Ok(())` on success.
async fn handle_client_message(
    raw: &str,
    state: &AppState,
    sender: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    user_id: Option<&UserId>,
    _display_user: &str,
) -> std::result::Result<(), bool> {
    // Parse the incoming message
    let ws_msg: WsChatMessage = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(e) => {
            let err_resp = WsChatResponse {
                msg_type: "error".into(),
                content: None,
                session_id: None,
                served_by: None,
                error: Some(format!("Invalid message format: {e}")),
                latency_ms: None,
            };
            if let Ok(json) = serde_json::to_string(&err_resp) {
                let _ = sender.send(WsMessage::Text(json.into())).await;
            }
            return Err(false);
        }
    };

    if ws_msg.message.trim().is_empty() {
        return Err(false);
    }

    // Get or create session
    let session_id = match state
        .sessions()
        .get_or_create(user_id, "websocket")
    {
        Ok(sid) => sid,
        Err(e) => {
            let err_resp = WsChatResponse {
                msg_type: "error".into(),
                content: None,
                session_id: None,
                served_by: None,
                error: Some(format!("Session error: {e}")),
                latency_ms: None,
            };
            if let Ok(json) = serde_json::to_string(&err_resp) {
                let _ = sender.send(WsMessage::Text(json.into())).await;
            }
            return Err(false);
        }
    };

    // Send "thinking" indicator
    let thinking = WsChatResponse {
        msg_type: "thinking".into(),
        content: None,
        session_id: Some(session_id.to_string()),
        served_by: None,
        error: None,
        latency_ms: None,
    };
    if let Ok(json) = serde_json::to_string(&thinking) {
        let _ = sender.send(WsMessage::Text(json.into())).await;
    }

    // Store in working memory
    state.memory().working.push(
        &session_id,
        ngenorca_memory::working::WorkingMessage {
            role: "user".into(),
            content: ws_msg.message.clone(),
            timestamp: chrono::Utc::now(),
            estimated_tokens: ws_msg.message.len() / 4,
        },
    );

    // Build conversation from working memory
    let working_messages = state.memory().working.get_session(&session_id);
    let conversation: Vec<ChatMessage> = working_messages
        .iter()
        .rev()
        .skip(1) // Skip the message we just added (it will be the current message)
        .rev()
        .map(|wm| ChatMessage {
            role: wm.role.clone(),
            content: wm.content.clone(),
        })
        .collect();

    // Orchestrate
    let orch = HybridOrchestrator::new(Arc::new(state.config().clone()));

    let response = match orch
        .process(&ws_msg.message, &conversation, state.providers(), Some(state.plugins()), None)
        .await
    {
        Ok((resp, record)) => {
            state.metrics().inc_orchestrations();
            state.metrics().add_tokens(resp.total_usage.total_tokens as u64);
            if resp.escalated {
                state.metrics().inc_escalations();
            }

            // Ingest into learned routing
            if let Err(e) = state.learned_router().ingest(&record) {
                warn!(error = %e, "Failed to ingest WS learned routing rule");
            }

            // Publish orchestration record
            let orch_event = ngenorca_core::event::Event {
                id: ngenorca_core::types::EventId::new(),
                timestamp: chrono::Utc::now(),
                session_id: Some(session_id.clone()),
                user_id: None,
                payload: ngenorca_core::event::EventPayload::OrchestrationCompleted(record),
            };
            if let Err(e) = state.event_bus().publish(orch_event).await {
                warn!(error = %e, "Failed to publish WS orchestration record");
            }

            // Store assistant response in working memory
            state.memory().working.push(
                &session_id,
                ngenorca_memory::working::WorkingMessage {
                    role: "assistant".into(),
                    content: resp.content.clone(),
                    timestamp: chrono::Utc::now(),
                    estimated_tokens: resp.content.len() / 4,
                },
            );

            let _ = state
                .sessions()
                .record_message(&session_id, resp.total_usage.total_tokens);

            WsChatResponse {
                msg_type: "response".into(),
                content: Some(resp.content),
                session_id: Some(session_id.to_string()),
                served_by: Some(format!(
                    "{}/{}",
                    resp.served_by.name, resp.served_by.model
                )),
                error: None,
                latency_ms: Some(resp.latency_ms),
            }
        }
        Err(e) => {
            state.metrics().inc_http_errors();
            error!(error = %e, "WebSocket chat failed");
            WsChatResponse {
                msg_type: "error".into(),
                content: None,
                session_id: Some(session_id.to_string()),
                served_by: None,
                error: Some(e.to_string()),
                latency_ms: None,
            }
        }
    };

    if let Ok(json) = serde_json::to_string(&response) {
        if sender.send(WsMessage::Text(json.into())).await.is_err() {
            return Err(true); // Client disconnected
        }
    }

    Ok(())
}

/// WebSocket push notification for bus events.
#[derive(Debug, Serialize)]
struct WsEventPush {
    msg_type: String,
    event_id: String,
    event_kind: String,
    session_id: Option<String>,
    payload: Option<serde_json::Value>,
    timestamp: String,
}

/// Return a human-readable kind label for an EventPayload.
fn event_payload_kind(payload: &ngenorca_core::event::EventPayload) -> String {
    use ngenorca_core::event::EventPayload;
    match payload {
        EventPayload::Message(_) => "message",
        EventPayload::SessionCreated { .. } => "session_created",
        EventPayload::SessionEnded { .. } => "session_ended",
        EventPayload::PluginLoaded { .. } => "plugin_loaded",
        EventPayload::PluginUnloaded { .. } => "plugin_unloaded",
        EventPayload::IdentityChange { .. } => "identity_change",
        EventPayload::MemoryUpdate { .. } => "memory_update",
        EventPayload::ToolExecution { .. } => "tool_execution",
        EventPayload::SystemLifecycle(_) => "system_lifecycle",
        EventPayload::OrchestrationCompleted(_) => "orchestration_completed",
    }
    .into()
}