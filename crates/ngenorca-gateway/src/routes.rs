//! HTTP routes and WebSocket handler.

use axum::{
    Extension, Router,
    extract::{
        Query, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    response::{IntoResponse, Json},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use ngenorca_config::NgenOrcaConfig;
use ngenorca_core::event::EventPayload;
use ngenorca_core::identity::{AttestationType, UserRole};
use ngenorca_core::orchestration::{QualityMethod, QualityVerdict, TaskIntent};
use ngenorca_core::types::{ChannelId, ChannelKind, DeviceId, SessionId, UserId};
use ngenorca_identity::resolver::IdentityAction;
use ngenorca_identity::{ChallengeSeed, PairingCompletion, PairingSeed};
use ngenorca_plugin_sdk::{ChatMessage, OrchestrationDiagnostics};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::auth::CallerIdentity;
use crate::orchestration::{HybridOrchestrator, InvocationContext};
use crate::runtime_identity::{self, IdentityResolutionDiagnostics};
use crate::state::AppState;

/// Build the main router with all routes.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/config", get(crate::config_ui::config_page))
        .route("/health", get(health))
        .route("/api/v1/status", get(status))
        .route("/api/v1/version", get(version))
        .route(
            "/api/v1/config/effective",
            get(crate::config_ui::get_effective_config),
        )
        .route(
            "/api/v1/config/file",
            get(crate::config_ui::get_config_file).put(crate::config_ui::save_config_file),
        )
        .route("/api/v1/whoami", get(whoami))
        .route("/api/v1/providers", get(providers))
        .route("/api/v1/channels", get(channels))
        .route("/api/v1/orchestration", get(orchestration_info))
        .route(
            "/api/v1/orchestration/learned",
            get(list_learned_routes).delete(clear_learned_routes),
        )
        .route(
            "/api/v1/orchestration/classify",
            axum::routing::post(classify_preview),
        )
        .route("/api/v1/identity/users", get(list_users))
        .route(
            "/api/v1/identity/pairing/start",
            post(start_identity_pairing),
        )
        .route(
            "/api/v1/identity/pairing/complete",
            post(complete_identity_pairing),
        )
        .route(
            "/api/v1/identity/challenge/start",
            post(start_identity_challenge),
        )
        .route(
            "/api/v1/identity/challenge/verify",
            post(verify_identity_challenge),
        )
        .route("/api/v1/memory/stats", get(memory_stats))
        .route(
            "/api/v1/memory/user/{user_id}",
            axum::routing::delete(delete_user_data),
        )
        .route("/api/v1/events/count", get(event_count))
        .route("/api/v1/events/history", get(event_history))
        .route(
            "/api/v1/events/timeline/corrections",
            get(correction_timeline),
        )
        // ── New endpoints ──
        .route("/api/v1/chat", post(chat))
        .route("/api/v1/sessions", get(list_sessions))
        // SEC-05: Channel webhook callback routes
        .route("/webhooks/{channel}", post(webhook_inbound))
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
            "config_ui": "/config",
            "config_file": "/api/v1/config/file",
            "config_effective": "/api/v1/config/effective",
            "status": "/api/v1/status",
            "whoami": "/api/v1/whoami",
            "chat": "POST /api/v1/chat",
            "sessions": "/api/v1/sessions",
            "websocket": "WS /ws",
            "providers": "/api/v1/providers",
            "channels": "/api/v1/channels",
            "orchestration": "/api/v1/orchestration",
            "orchestration_learned": "/api/v1/orchestration/learned",
            "classify": "POST /api/v1/orchestration/classify",
            "users": "/api/v1/identity/users",
            "identity_pairing_start": "POST /api/v1/identity/pairing/start",
            "identity_pairing_complete": "POST /api/v1/identity/pairing/complete",
            "identity_challenge_start": "POST /api/v1/identity/challenge/start",
            "identity_challenge_verify": "POST /api/v1/identity/challenge/verify",
            "memory": "/api/v1/memory/stats",
            "events": "/api/v1/events/count",
            "event_history": "/api/v1/events/history",
            "correction_timeline": "/api/v1/events/timeline/corrections",
        },
    }))
}

/// Health check endpoint (no auth required — used by nginx/Docker healthcheck).
async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    let uptime = state.uptime();

    Json(json!({
        "status": "healthy",
        "uptime_secs": uptime.as_secs(),
        "sandbox": sandbox_payload(state.config()),
    }))
}

/// Lightweight version endpoint — returns only the crate version string.
async fn version() -> Json<serde_json::Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Who am I? Shows the caller's identity as resolved by the auth middleware.
/// Useful for verifying Authelia → nginx → NgenOrca identity flow.
async fn whoami(
    State(state): State<AppState>,
    Extension(caller): Extension<CallerIdentity>,
) -> Json<serde_json::Value> {
    let resolved = runtime_identity::resolve_web_identity(state.identity(), &caller);
    let identity = runtime_identity::describe_web_identity(&caller, &resolved, None);
    Json(json!({
        "username": caller.username,
        "email": caller.email,
        "groups": caller.groups,
        "auth_method": format!("{:?}", caller.auth_method),
        "identity": identity,
    }))
}

/// Status endpoint with system overview.
async fn status(
    State(state): State<AppState>,
    Extension(caller): Extension<CallerIdentity>,
) -> Json<serde_json::Value> {
    let uptime = state.uptime();
    let event_count = state.event_bus().event_count().unwrap_or(0);
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
            "retention_days": state.config().gateway.event_log_retention_days,
            "prune_interval_secs": state.config().gateway.event_log_prune_interval_secs,
            "history_endpoints": [
                "/api/v1/events/history",
                "/api/v1/events/timeline/corrections"
            ],
        },
        "memory": {
            "enabled": state.config().memory.enabled,
        },
        "sandbox": sandbox_payload(state.config()),
    }))
}

fn sandbox_payload(config: &NgenOrcaConfig) -> serde_json::Value {
    let policy = sandbox_policy_from_config(config);
    json!({
        "enabled": config.sandbox.enabled,
        "requested_backend": format!("{:?}", config.sandbox.backend),
        "environment": format!("{:?}", ngenorca_sandbox::detect_environment()),
        "policy": {
            "allow_network": config.sandbox.policy.allow_network,
            "allow_workspace_write": config.sandbox.policy.allow_workspace_write,
            "allow_child_processes": config.sandbox.policy.allow_child_processes,
            "workspace_root": config.agent.workspace.to_string_lossy().to_string(),
            "additional_read_paths": config.sandbox.policy.additional_read_paths.clone(),
            "additional_write_paths": config.sandbox.policy.additional_write_paths.clone(),
            "memory_limit_mb": config.sandbox.policy.memory_limit_mb,
            "cpu_limit_seconds": config.sandbox.policy.cpu_limit_seconds,
            "wall_time_limit_seconds": config.sandbox.policy.wall_time_limit_seconds,
        },
        "audit": ngenorca_sandbox::audit_policy(&policy, config.sandbox.enabled),
    })
}

fn sandbox_policy_from_config(config: &NgenOrcaConfig) -> ngenorca_sandbox::SandboxPolicy {
    let workspace_root = config.agent.workspace.to_string_lossy().to_string();
    let mut allow_read_paths = vec![workspace_root.clone()];
    allow_read_paths.extend(config.sandbox.policy.additional_read_paths.clone());

    let mut allow_write_paths = config.sandbox.policy.additional_write_paths.clone();
    if config.sandbox.policy.allow_workspace_write {
        allow_write_paths.insert(0, workspace_root);
    }

    ngenorca_sandbox::SandboxPolicy {
        allow_network: config.sandbox.policy.allow_network,
        allow_read_paths,
        allow_write_paths,
        allow_spawn: config.sandbox.policy.allow_child_processes,
        memory_limit_bytes: config
            .sandbox
            .policy
            .memory_limit_mb
            .saturating_mul(1024 * 1024),
        cpu_time_limit_secs: config.sandbox.policy.cpu_limit_seconds,
        wall_timeout_secs: config.sandbox.policy.wall_time_limit_seconds,
    }
}

/// Show configured LLM providers and which is active.
async fn providers(State(state): State<AppState>) -> Json<serde_json::Value> {
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
async fn channels(State(state): State<AppState>) -> Json<serde_json::Value> {
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

#[derive(Debug, Deserialize)]
struct PairingStartRequest {
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    handle: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PairingCompleteRequest {
    pairing_id: String,
    user_id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    attestation: Option<String>,
    #[serde(default)]
    public_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChallengeStartRequest {
    device_id: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChallengeVerifyRequest {
    challenge_id: String,
    signature: String,
    #[serde(default)]
    session_id: Option<String>,
}

async fn start_identity_pairing(
    State(state): State<AppState>,
    Extension(caller): Extension<CallerIdentity>,
    Json(body): Json<PairingStartRequest>,
) -> Json<serde_json::Value> {
    let handle = body.handle.or_else(|| default_web_handle(&caller));
    let device_id = body.device_id.map(DeviceId);
    if handle.is_none() && device_id.is_none() {
        return Json(json!({
            "error": "pairing start requires at least a handle or device_id",
        }));
    }

    let channel_kind = match body.channel.as_deref() {
        Some(value) => match parse_channel_kind(value) {
            Ok(value) => Some(value),
            Err(error) => return Json(json!({ "error": error })),
        },
        None => Some(ChannelKind::WebChat),
    };

    let request = match state.identity().issue_pairing_request(PairingSeed {
        channel_kind,
        handle,
        device_id,
        device_name: body.device_name,
        requested_user_id: body.user_id.map(UserId),
        requested_display_name: body.display_name,
    }) {
        Ok(request) => request,
        Err(error) => return Json(json!({ "error": error.to_string() })),
    };

    Json(json!({
        "pairing_id": request.request_id,
        "channel": request.channel_kind.as_ref().map(channel_kind_label),
        "handle": request.handle,
        "device_id": request.device_id.as_ref().map(|value| value.0.clone()),
        "device_name": request.device_name,
        "requested_user_id": request.requested_user_id.as_ref().map(|value| value.0.clone()),
        "requested_display_name": request.requested_display_name,
        "created_at": request.created_at.to_rfc3339(),
        "expires_at": request.expires_at.to_rfc3339(),
        "next": {
            "complete": "/api/v1/identity/pairing/complete"
        }
    }))
}

async fn complete_identity_pairing(
    State(state): State<AppState>,
    Json(body): Json<PairingCompleteRequest>,
) -> Json<serde_json::Value> {
    let role = match body.role.as_deref().map(parse_user_role).transpose() {
        Ok(Some(role)) => role,
        Ok(None) => UserRole::Owner,
        Err(error) => return Json(json!({ "error": error })),
    };
    let attestation = match body
        .attestation
        .as_deref()
        .map(parse_attestation_type)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => return Json(json!({ "error": error })),
    };

    let user = match state.identity().complete_pairing_request(
        &body.pairing_id,
        PairingCompletion {
            user_id: UserId(body.user_id.clone()),
            display_name: body.display_name.clone(),
            role,
            channel_id: body.channel_id.clone().map(ChannelId),
            device_name: body.device_name.clone(),
            attestation,
            public_key: body.public_key.clone(),
        },
    ) {
        Ok(user) => user,
        Err(error) => return Json(json!({ "error": error.to_string() })),
    };

    let rebound_session_id = body
        .session_id
        .as_deref()
        .and_then(parse_session_id)
        .and_then(|session_id| match state.sessions().promote_to_user(&session_id, &user.user_id) {
            Ok(promoted) => Some(promoted.to_string()),
            Err(error) => {
                warn!(error = %error, session_id = %session_id, "Failed to rebind paired session");
                None
            }
        });

    Json(json!({
        "paired": true,
        "user": {
            "user_id": user.user_id.0,
            "display_name": user.display_name,
            "role": format!("{:?}", user.role),
            "devices": user.devices.len(),
            "channels": user.channels.len(),
        },
        "session_id": rebound_session_id,
    }))
}

async fn start_identity_challenge(
    State(state): State<AppState>,
    Json(body): Json<ChallengeStartRequest>,
) -> Json<serde_json::Value> {
    let device_id = DeviceId(body.device_id.clone());
    let known_user = match state.identity().resolve_by_device(&device_id) {
        Ok(Some((user_id, _))) => Some(user_id),
        Ok(None) => None,
        Err(error) => return Json(json!({ "error": error.to_string() })),
    };
    if known_user.is_none() {
        return Json(json!({
            "error": format!("unknown device {}", body.device_id),
        }));
    }

    let challenge = match state.identity().issue_challenge_request(ChallengeSeed {
        device_id,
        user_id: known_user,
        reason: body
            .reason
            .unwrap_or_else(|| "runtime identity verification retry".into()),
    }) {
        Ok(challenge) => challenge,
        Err(error) => return Json(json!({ "error": error.to_string() })),
    };

    Json(json!({
        "challenge_id": challenge.request_id,
        "device_id": challenge.device_id.0,
        "user_id": challenge.user_id.as_ref().map(|value| value.0.clone()),
        "nonce_b64": challenge.nonce_b64,
        "reason": challenge.reason,
        "expires_at": challenge.expires_at.to_rfc3339(),
        "next": {
            "verify": "/api/v1/identity/challenge/verify"
        }
    }))
}

async fn verify_identity_challenge(
    State(state): State<AppState>,
    Extension(caller): Extension<CallerIdentity>,
    Json(body): Json<ChallengeVerifyRequest>,
) -> Json<serde_json::Value> {
    let (user_id, trust) = match state
        .identity()
        .verify_challenge_response(&body.challenge_id, &body.signature)
    {
        Ok(result) => result,
        Err(error) => return Json(json!({ "error": error.to_string() })),
    };

    let linked_handles =
        runtime_identity::link_authenticated_web_handles(state.identity(), &user_id, &caller);

    let rebound_session_id = body
        .session_id
        .as_deref()
        .and_then(parse_session_id)
        .and_then(|session_id| match state.sessions().promote_to_user(&session_id, &user_id) {
            Ok(promoted) => Some(promoted.to_string()),
            Err(error) => {
                warn!(error = %error, session_id = %session_id, "Failed to rebind challenged session");
                None
            }
        });

    Json(json!({
        "verified": true,
        "user_id": user_id.0,
        "trust": format!("{:?}", trust),
        "linked_handles": linked_handles,
        "session_id": rebound_session_id,
    }))
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

/// PRIV-03: Delete all stored data for a user.
///
/// `DELETE /api/v1/memory/user/:user_id`
///
/// Purges episodic and semantic memory tiers. Working memory is session-keyed
/// and expires automatically.
///
/// Authorization: only the user themselves may delete their own data.
/// A future admin role could bypass this restriction.
async fn delete_user_data(
    State(state): State<AppState>,
    axum::extract::Path(user_id): axum::extract::Path<String>,
    Extension(caller): Extension<CallerIdentity>,
) -> impl IntoResponse {
    // Must be authenticated.
    let caller_name = match &caller.username {
        Some(name) if !name.is_empty() => name.clone(),
        _ => {
            return (
                axum::http::StatusCode::FORBIDDEN,
                Json(json!({ "error": "Authentication required for data deletion" })),
            );
        }
    };

    // Authorization: callers may only delete their own data,
    // unless they hold the Owner role (IAM-01: admin bypass).
    let caller_is_owner = state
        .identity()
        .get_user(&ngenorca_core::types::UserId(caller_name.clone()))
        .ok()
        .and_then(|opt| opt)
        .is_some_and(|u| u.role == UserRole::Owner);
    if caller_name != user_id && !caller_is_owner {
        warn!(
            caller = %caller_name,
            target = %user_id,
            "Unauthorized data deletion attempt — caller is not the target user and is not an Owner"
        );
        return (
            axum::http::StatusCode::FORBIDDEN,
            Json(json!({ "error": "You may only delete your own data" })),
        );
    }

    let uid = ngenorca_core::types::UserId(user_id.clone());

    match state.memory().delete_user_data(&uid) {
        Ok(report) => {
            info!(user = %user_id, "User data deleted (PRIV-03) by {:?}", caller.username);
            (
                axum::http::StatusCode::OK,
                Json(json!({
                    "deleted": true,
                    "user_id": user_id,
                    "episodic_entries_deleted": report.episodic_deleted,
                    "semantic_facts_deleted": report.semantic_deleted,
                    "working_memory_note": report.working_note,
                })),
            )
        }
        Err(e) => {
            error!(user = %user_id, error = %e, "Failed to delete user data");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Deletion failed: {e}") })),
            )
        }
    }
}

/// Event count.
async fn event_count(State(state): State<AppState>) -> Json<serde_json::Value> {
    let count = state.event_bus().event_count().unwrap_or(0);
    Json(json!({
        "total_events": count,
        "retention_days": state.config().gateway.event_log_retention_days,
        "prune_interval_secs": state.config().gateway.event_log_prune_interval_secs,
    }))
}

const OPERATOR_HISTORY_WINDOW: usize = 200;
const DEFAULT_EVENT_HISTORY_LIMIT: usize = 250;
const MAX_EVENT_HISTORY_LIMIT: usize = 2000;
const DEFAULT_TIMELINE_WINDOW_HOURS: i64 = 24 * 14;
const DEFAULT_TIMELINE_BUCKET_HOURS: i64 = 24;
const MAX_TIMELINE_BUCKETS: usize = 180;

#[derive(Debug, Deserialize, Default)]
struct EventHistoryQuery {
    limit: Option<usize>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    target_agent: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    failure_only: Option<bool>,
    #[serde(default)]
    since_hours: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
struct CorrectionTimelineQuery {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    since_hours: Option<i64>,
    #[serde(default)]
    bucket_hours: Option<i64>,
}

#[derive(Debug, Default, Clone, Serialize)]
struct OperatorHistorySummary {
    window_size: usize,
    orchestration_count: usize,
    tool_event_count: usize,
    avg_latency_ms: f64,
    avg_total_tokens: f64,
    escalation_rate: f64,
    augment_rate: f64,
    learned_route_reuse_rate: f64,
    auto_accept_rate: f64,
    tool_failure_rate: f64,
    correction_attempt_rate: f64,
    grounded_response_rate: f64,
    remediation_success_rate: f64,
    post_synthesis_drift_correction_rate: f64,
    intent_mix: Vec<HistoryBucket>,
    agent_mix: Vec<HistoryBucket>,
    quality_mix: Vec<HistoryBucket>,
    user_mix: Vec<HistoryBucket>,
    channel_mix: Vec<HistoryBucket>,
    tool_mix: Vec<HistoryBucket>,
    recent_failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HistoryEventView {
    event_id: String,
    timestamp: String,
    event_kind: String,
    session_id: Option<String>,
    user_id: Option<String>,
    channel: Option<String>,
    summary: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
struct CorrectionTimelineBucket {
    start: String,
    end: String,
    orchestration_count: usize,
    tool_event_count: usize,
    escalations: usize,
    correction_attempts: usize,
    remediation_successes: usize,
    drift_corrections: usize,
    grounded_responses: usize,
    tool_failures: usize,
}

#[derive(Debug, Default, Clone)]
struct CorrectionTimelineAccumulator {
    orchestration_count: usize,
    tool_event_count: usize,
    escalations: usize,
    correction_attempts: usize,
    remediation_successes: usize,
    drift_corrections: usize,
    grounded_responses: usize,
    tool_failures: usize,
}

#[derive(Debug, Default, Clone)]
struct HistoryAccumulator {
    count: usize,
    total_latency_ms: u64,
    total_tokens: usize,
    escalations: usize,
    augments: usize,
    learned_reuse: usize,
    auto_accepts: usize,
    tool_events: usize,
    tool_failures: usize,
    correction_attempts: usize,
    grounded_responses: usize,
    remediation_successes: usize,
    post_synthesis_drift_corrections: usize,
    recent_failures: Vec<String>,
    intents: BTreeMap<String, usize>,
    agents: BTreeMap<String, usize>,
    quality: BTreeMap<String, usize>,
    users: BTreeMap<String, usize>,
    channels: BTreeMap<String, usize>,
    tools: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
struct HistoryBucket {
    label: String,
    count: usize,
}

fn recent_operator_history(state: &AppState, limit: usize) -> OperatorHistorySummary {
    let events = state.event_bus().replay_recent(limit).unwrap_or_default();
    operator_history_from_events(&events, limit)
}

fn operator_history_from_events(
    events: &[ngenorca_core::event::Event],
    window_size: usize,
) -> OperatorHistorySummary {
    let mut acc = HistoryAccumulator::default();

    for event in events {
        match &event.payload {
            EventPayload::OrchestrationCompleted(record) => {
                acc.count += 1;
                acc.total_latency_ms += record.latency_ms;
                acc.total_tokens += record.total_tokens;
                acc.learned_reuse += usize::from(record.routing.from_memory);
                acc.auto_accepts +=
                    usize::from(matches!(record.quality_method, QualityMethod::AutoAccept));
                acc.correction_attempts += usize::from(record.correction.verification_attempted);
                acc.grounded_responses += usize::from(record.correction.grounded);
                acc.remediation_successes += usize::from(record.correction.remediation_succeeded);
                acc.post_synthesis_drift_corrections +=
                    usize::from(record.correction.post_synthesis_drift_corrected);

                increment_bucket(
                    &mut acc.intents,
                    format!("{:?}", record.classification.intent),
                );
                increment_bucket(&mut acc.agents, record.routing.target.name.clone());
                increment_bucket(&mut acc.quality, quality_label(&record.quality).into());
                if let Some(user_id) = record.user_id.as_ref() {
                    increment_bucket(&mut acc.users, user_id.to_string());
                }
                if let Some(channel) = record.channel.as_ref() {
                    increment_bucket(&mut acc.channels, channel.clone());
                }

                if record.escalated {
                    acc.escalations += 1;
                }
                if matches!(record.quality, QualityVerdict::Augment { .. }) {
                    acc.augments += 1;
                }
            }
            EventPayload::ToolExecution {
                tool_name,
                success,
                failure_class,
                ..
            } => {
                acc.tool_events += 1;
                increment_bucket(&mut acc.tools, tool_name.clone());
                if matches!(success, Some(false)) {
                    acc.tool_failures += 1;
                    if acc.recent_failures.len() < 8 {
                        acc.recent_failures.push(
                            failure_class
                                .as_ref()
                                .map(|class| format!("{tool_name}:{class}"))
                                .unwrap_or_else(|| tool_name.to_string()),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let orchestration_divisor = acc.count.max(1) as f64;
    let tool_divisor = acc.tool_events.max(1) as f64;

    OperatorHistorySummary {
        window_size,
        orchestration_count: acc.count,
        tool_event_count: acc.tool_events,
        avg_latency_ms: acc.total_latency_ms as f64 / orchestration_divisor,
        avg_total_tokens: acc.total_tokens as f64 / orchestration_divisor,
        escalation_rate: acc.escalations as f64 / orchestration_divisor,
        augment_rate: acc.augments as f64 / orchestration_divisor,
        learned_route_reuse_rate: acc.learned_reuse as f64 / orchestration_divisor,
        auto_accept_rate: acc.auto_accepts as f64 / orchestration_divisor,
        tool_failure_rate: acc.tool_failures as f64 / tool_divisor,
        correction_attempt_rate: acc.correction_attempts as f64 / orchestration_divisor,
        grounded_response_rate: acc.grounded_responses as f64 / orchestration_divisor,
        remediation_success_rate: acc.remediation_successes as f64 / orchestration_divisor,
        post_synthesis_drift_correction_rate: acc.post_synthesis_drift_corrections as f64
            / orchestration_divisor,
        intent_mix: buckets_from_map(acc.intents),
        agent_mix: buckets_from_map(acc.agents),
        quality_mix: buckets_from_map(acc.quality),
        user_mix: buckets_from_map(acc.users),
        channel_mix: buckets_from_map(acc.channels),
        tool_mix: buckets_from_map(acc.tools),
        recent_failures: acc.recent_failures,
    }
}

async fn event_history(
    State(state): State<AppState>,
    Query(query): Query<EventHistoryQuery>,
) -> impl IntoResponse {
    let requested_limit = query
        .limit
        .unwrap_or(DEFAULT_EVENT_HISTORY_LIMIT)
        .clamp(1, MAX_EVENT_HISTORY_LIMIT);
    let overfetch_limit = if operator_query_needs_payload_filter(&query) {
        requested_limit
            .saturating_mul(4)
            .min(MAX_EVENT_HISTORY_LIMIT)
    } else {
        requested_limit
    };

    let base_query = match build_event_query(&query, overfetch_limit) {
        Ok(value) => value,
        Err(message) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": message })),
            )
                .into_response();
        }
    };

    let parsed_intent = match query.intent.as_deref().map(parse_task_intent).transpose() {
        Ok(value) => value,
        Err(message) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": message })),
            )
                .into_response();
        }
    };

    let events = state
        .event_bus()
        .replay_filtered(&base_query)
        .unwrap_or_default();
    let filtered = events
        .into_iter()
        .filter(|event| matches_history_filters(event, &query, parsed_intent.as_ref()))
        .collect::<Vec<_>>();
    let history = operator_history_from_events(&filtered, filtered.len());
    let views = filtered
        .iter()
        .rev()
        .take(requested_limit)
        .map(history_event_view)
        .collect::<Vec<_>>();

    Json(json!({
        "filters": {
            "limit": requested_limit,
            "session_id": query.session_id,
            "user_id": query.user_id,
            "channel": query.channel,
            "kind": query.kind,
            "intent": query.intent,
            "target_agent": query.target_agent,
            "tool_name": query.tool_name,
            "failure_only": query.failure_only.unwrap_or(false),
            "since_hours": query.since_hours,
        },
        "matched_events": filtered.len(),
        "returned_events": views.len(),
        "history": history,
        "events": views,
    }))
    .into_response()
}

async fn correction_timeline(
    State(state): State<AppState>,
    Query(query): Query<CorrectionTimelineQuery>,
) -> impl IntoResponse {
    let bucket_hours = query
        .bucket_hours
        .unwrap_or(DEFAULT_TIMELINE_BUCKET_HOURS)
        .clamp(1, 24 * 14);
    let since_hours = query
        .since_hours
        .unwrap_or(DEFAULT_TIMELINE_WINDOW_HOURS)
        .clamp(1, 24 * 365);
    let max_events = MAX_EVENT_HISTORY_LIMIT;

    let base_query = match build_timeline_event_query(&query, since_hours, max_events) {
        Ok(value) => value,
        Err(message) => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": message })),
            )
                .into_response();
        }
    };

    let events = state
        .event_bus()
        .replay_filtered(&base_query)
        .unwrap_or_default()
        .into_iter()
        .filter(|event| matches_timeline_filters(event, &query))
        .collect::<Vec<_>>();

    let buckets = build_correction_timeline(&events, bucket_hours);

    Json(json!({
        "filters": {
            "session_id": query.session_id,
            "user_id": query.user_id,
            "channel": query.channel,
            "since_hours": since_hours,
            "bucket_hours": bucket_hours,
        },
        "event_count": events.len(),
        "bucket_count": buckets.len(),
        "buckets": buckets,
    }))
    .into_response()
}

fn operator_query_needs_payload_filter(query: &EventHistoryQuery) -> bool {
    query.channel.is_some()
        || query.kind.is_some()
        || query.intent.is_some()
        || query.target_agent.is_some()
        || query.tool_name.is_some()
        || query.failure_only.unwrap_or(false)
}

fn build_event_query(
    query: &EventHistoryQuery,
    limit: usize,
) -> Result<ngenorca_bus::EventQuery, String> {
    Ok(ngenorca_bus::EventQuery {
        session_id: parse_session_filter(query.session_id.as_deref())?,
        user_id: query.user_id.clone().map(UserId),
        since: query
            .since_hours
            .map(|hours| chrono::Utc::now() - chrono::Duration::hours(hours.max(1))),
        until: None,
        limit: Some(limit),
    })
}

fn build_timeline_event_query(
    query: &CorrectionTimelineQuery,
    since_hours: i64,
    limit: usize,
) -> Result<ngenorca_bus::EventQuery, String> {
    Ok(ngenorca_bus::EventQuery {
        session_id: parse_session_filter(query.session_id.as_deref())?,
        user_id: query.user_id.clone().map(UserId),
        since: Some(chrono::Utc::now() - chrono::Duration::hours(since_hours)),
        until: None,
        limit: Some(limit),
    })
}

fn parse_session_filter(value: Option<&str>) -> Result<Option<SessionId>, String> {
    match value {
        Some(raw) => parse_session_id(raw)
            .map(Some)
            .ok_or_else(|| format!("invalid session_id '{raw}'")),
        None => Ok(None),
    }
}

fn matches_history_filters(
    event: &ngenorca_core::event::Event,
    query: &EventHistoryQuery,
    parsed_intent: Option<&TaskIntent>,
) -> bool {
    if let Some(kind) = query.kind.as_ref()
        && !event_payload_kind(&event.payload).eq_ignore_ascii_case(kind)
    {
        return false;
    }

    if let Some(channel) = query.channel.as_ref() {
        let event_channel = operator_event_channel(event);
        if !event_channel
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(channel))
        {
            return false;
        }
    }

    if let Some(tool_name) = query.tool_name.as_ref() {
        match &event.payload {
            EventPayload::ToolExecution {
                tool_name: actual, ..
            } if actual.eq_ignore_ascii_case(tool_name) => {}
            EventPayload::ToolExecution { .. } => return false,
            _ => return false,
        }
    }

    if let Some(target_agent) = query.target_agent.as_ref() {
        match &event.payload {
            EventPayload::OrchestrationCompleted(record)
                if record
                    .routing
                    .target
                    .name
                    .eq_ignore_ascii_case(target_agent) => {}
            EventPayload::OrchestrationCompleted(_) => return false,
            _ => return false,
        }
    }

    if let Some(intent) = parsed_intent {
        match &event.payload {
            EventPayload::OrchestrationCompleted(record)
                if record.classification.intent == *intent => {}
            EventPayload::OrchestrationCompleted(_) => return false,
            _ => return false,
        }
    }

    if query.failure_only.unwrap_or(false) {
        match &event.payload {
            EventPayload::ToolExecution { success, .. } => {
                if !matches!(success, Some(false)) {
                    return false;
                }
            }
            EventPayload::OrchestrationCompleted(record) => {
                if !(record.escalated
                    || record.correction.had_failures
                    || record.correction.had_blocked_calls)
                {
                    return false;
                }
            }
            _ => return false,
        }
    }

    true
}

fn matches_timeline_filters(
    event: &ngenorca_core::event::Event,
    query: &CorrectionTimelineQuery,
) -> bool {
    if let Some(channel) = query.channel.as_ref() {
        let event_channel = operator_event_channel(event);
        if !event_channel
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(channel))
        {
            return false;
        }
    }

    matches!(
        &event.payload,
        EventPayload::OrchestrationCompleted(_) | EventPayload::ToolExecution { .. }
    )
}

fn operator_event_channel(event: &ngenorca_core::event::Event) -> Option<String> {
    match &event.payload {
        EventPayload::OrchestrationCompleted(record) => record.channel.clone(),
        EventPayload::ToolExecution { channel, .. } => channel.clone(),
        EventPayload::Message(message) => Some(channel_kind_label(&message.channel_kind)),
        EventPayload::SystemLifecycle(ngenorca_core::event::LifecycleEvent::AdapterConnected {
            channel,
        })
        | EventPayload::SystemLifecycle(
            ngenorca_core::event::LifecycleEvent::AdapterDisconnected { channel, .. },
        ) => Some(channel.clone()),
        _ => None,
    }
}

fn history_event_view(event: &ngenorca_core::event::Event) -> HistoryEventView {
    HistoryEventView {
        event_id: event.id.to_string(),
        timestamp: event.timestamp.to_rfc3339(),
        event_kind: event_payload_kind(&event.payload),
        session_id: event.session_id.as_ref().map(ToString::to_string),
        user_id: event.user_id.as_ref().map(ToString::to_string),
        channel: operator_event_channel(event),
        summary: match &event.payload {
            EventPayload::OrchestrationCompleted(record) => json!({
                "intent": format!("{:?}", record.classification.intent),
                "target_agent": record.routing.target.name,
                "quality": quality_label(&record.quality),
                "escalated": record.escalated,
                "latency_ms": record.latency_ms,
                "total_tokens": record.total_tokens,
                "correction": {
                    "verification_attempted": record.correction.verification_attempted,
                    "had_failures": record.correction.had_failures,
                    "had_blocked_calls": record.correction.had_blocked_calls,
                    "grounded": record.correction.grounded,
                    "remediation_attempted": record.correction.remediation_attempted,
                    "remediation_succeeded": record.correction.remediation_succeeded,
                    "post_synthesis_drift_corrected": record.correction.post_synthesis_drift_corrected,
                }
            }),
            EventPayload::ToolExecution {
                tool_name,
                duration_ms,
                success,
                failure_class,
                outcome,
                ..
            } => json!({
                "tool_name": tool_name,
                "duration_ms": duration_ms,
                "success": success,
                "failure_class": failure_class,
                "outcome": outcome,
            }),
            _ => serde_json::to_value(&event.payload)
                .unwrap_or_else(|_| json!({ "error": "serialization failed" })),
        },
    }
}

fn build_correction_timeline(
    events: &[ngenorca_core::event::Event],
    bucket_hours: i64,
) -> Vec<CorrectionTimelineBucket> {
    let bucket_seconds = bucket_hours.max(1) * 3600;
    let mut buckets = BTreeMap::<i64, CorrectionTimelineAccumulator>::new();

    for event in events {
        let bucket_start = (event.timestamp.timestamp() / bucket_seconds) * bucket_seconds;
        let acc = buckets.entry(bucket_start).or_default();
        match &event.payload {
            EventPayload::OrchestrationCompleted(record) => {
                acc.orchestration_count += 1;
                acc.escalations += usize::from(record.escalated);
                acc.correction_attempts += usize::from(record.correction.verification_attempted);
                acc.remediation_successes += usize::from(record.correction.remediation_succeeded);
                acc.drift_corrections +=
                    usize::from(record.correction.post_synthesis_drift_corrected);
                acc.grounded_responses += usize::from(record.correction.grounded);
            }
            EventPayload::ToolExecution { success, .. } => {
                acc.tool_event_count += 1;
                acc.tool_failures += usize::from(matches!(success, Some(false)));
            }
            _ => {}
        }
    }

    let mut timeline = buckets
        .into_iter()
        .map(|(start_ts, acc)| {
            let start = chrono::DateTime::<chrono::Utc>::from_timestamp(start_ts, 0)
                .unwrap_or_else(chrono::Utc::now);
            let end = start + chrono::Duration::hours(bucket_hours.max(1));
            CorrectionTimelineBucket {
                start: start.to_rfc3339(),
                end: end.to_rfc3339(),
                orchestration_count: acc.orchestration_count,
                tool_event_count: acc.tool_event_count,
                escalations: acc.escalations,
                correction_attempts: acc.correction_attempts,
                remediation_successes: acc.remediation_successes,
                drift_corrections: acc.drift_corrections,
                grounded_responses: acc.grounded_responses,
                tool_failures: acc.tool_failures,
            }
        })
        .collect::<Vec<_>>();
    if timeline.len() > MAX_TIMELINE_BUCKETS {
        timeline = timeline.split_off(timeline.len() - MAX_TIMELINE_BUCKETS);
    }
    timeline
}

fn increment_bucket(map: &mut BTreeMap<String, usize>, label: String) {
    *map.entry(label).or_insert(0) += 1;
}

fn buckets_from_map(map: BTreeMap<String, usize>) -> Vec<HistoryBucket> {
    let mut buckets = map
        .into_iter()
        .map(|(label, count)| HistoryBucket { label, count })
        .collect::<Vec<_>>();
    buckets.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.label.cmp(&right.label))
    });
    buckets
}

fn quality_label(quality: &QualityVerdict) -> &'static str {
    match quality {
        QualityVerdict::Accept { .. } => "accept",
        QualityVerdict::Escalate { .. } => "escalate",
        QualityVerdict::Augment { .. } => "augment",
    }
}

/// Show orchestration configuration and sub-agents.
async fn orchestration_info(State(state): State<AppState>) -> Json<serde_json::Value> {
    let orch = HybridOrchestrator::new(std::sync::Arc::new(state.config().clone()));
    let info = orch.info();
    let mut payload =
        serde_json::to_value(info).unwrap_or_else(|_| json!({ "error": "serialization failed" }));
    let policy = &state.config().agent.learned_routing;
    let diagnostics = state
        .learned_router()
        .diagnostics_with_policy(policy, policy.diagnostics_include_penalized)
        .unwrap_or_default();
    let all_diagnostics = state
        .learned_router()
        .diagnostics_with_policy(policy, true)
        .unwrap_or_default();
    let history = state
        .learned_router()
        .history_summary_with_policy(policy, true)
        .unwrap_or_else(|_| {
            serde_json::from_value(json!({
                "total_rules": 0,
                "eligible_rules": 0,
                "penalized_rules": 0,
                "stale_rules": 0,
                "intents": [],
                "agents": [],
                "domains": []
            }))
            .unwrap()
        });

    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "learned_routes".into(),
            json!({
                "count": diagnostics.len(),
                "summary": {
                    "total_known": all_diagnostics.len(),
                    "eligible": all_diagnostics.iter().filter(|diagnostic| !diagnostic.stale && diagnostic.effective_confidence >= policy.min_effective_confidence && diagnostic.rule.sample_count >= policy.min_samples).count(),
                    "stale": all_diagnostics.iter().filter(|diagnostic| diagnostic.stale).count(),
                    "penalized": all_diagnostics.iter().filter(|diagnostic| diagnostic.effective_confidence < policy.min_effective_confidence || diagnostic.rule.sample_count < policy.min_samples).count(),
                },
                "history": history,
                "policy": policy,
                "rules": diagnostics,
            }),
        );
        object.insert(
            "recent_history".into(),
            serde_json::to_value(recent_operator_history(&state, OPERATOR_HISTORY_WINDOW))
                .unwrap_or_else(|_| json!({ "error": "serialization failed" })),
        );
    }

    Json(payload)
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
            let routing = orch.route_with_learned(&classification, Some(state.learned_router()));
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

#[derive(Debug, Deserialize, Default)]
struct LearnedRouteQuery {
    include_penalized: Option<bool>,
    intent: Option<String>,
    target_agent: Option<String>,
    stale_only: Option<bool>,
}

async fn list_learned_routes(
    State(state): State<AppState>,
    Query(query): Query<LearnedRouteQuery>,
) -> impl IntoResponse {
    let policy = &state.config().agent.learned_routing;
    let include_penalized = query
        .include_penalized
        .unwrap_or(policy.diagnostics_include_penalized);
    let parsed_intent = match query.intent.as_deref() {
        Some(intent) => match parse_task_intent(intent) {
            Ok(intent) => Some(intent),
            Err(message) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(json!({ "error": message })),
                )
                    .into_response();
            }
        },
        None => None,
    };

    let diagnostics = state
        .learned_router()
        .diagnostics_with_policy(policy, include_penalized)
        .unwrap_or_default()
        .into_iter()
        .filter(|diagnostic| {
            parsed_intent
                .as_ref()
                .is_none_or(|intent| diagnostic.rule.intent == *intent)
        })
        .filter(|diagnostic| {
            query.target_agent.as_ref().is_none_or(|target_agent| {
                diagnostic
                    .rule
                    .target_agent
                    .eq_ignore_ascii_case(target_agent)
            })
        })
        .filter(|diagnostic| !query.stale_only.unwrap_or(false) || diagnostic.stale)
        .collect::<Vec<_>>();
    let history = state
        .learned_router()
        .history_summary_with_policy(policy, include_penalized)
        .unwrap_or_else(|_| {
            serde_json::from_value(json!({
                "total_rules": 0,
                "eligible_rules": 0,
                "penalized_rules": 0,
                "stale_rules": 0,
                "intents": [],
                "agents": [],
                "domains": []
            }))
            .unwrap()
        });

    Json(json!({
        "count": diagnostics.len(),
        "history": history,
        "policy": policy,
        "include_penalized": include_penalized,
        "filters": {
            "intent": query.intent,
            "target_agent": query.target_agent,
            "stale_only": query.stale_only,
        },
        "rules": diagnostics,
    }))
    .into_response()
}

async fn clear_learned_routes(
    State(state): State<AppState>,
    Query(query): Query<LearnedRouteQuery>,
) -> impl IntoResponse {
    let parsed_intent = match query.intent.as_deref() {
        Some(intent) => match parse_task_intent(intent) {
            Ok(intent) => Some(intent),
            Err(message) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(json!({ "error": message })),
                );
            }
        },
        None => None,
    };

    match state
        .learned_router()
        .delete_rules(parsed_intent.as_ref(), query.target_agent.as_deref())
    {
        Ok(deleted) => (
            axum::http::StatusCode::OK,
            Json(json!({
                "deleted": deleted,
                "filters": {
                    "intent": query.intent,
                    "target_agent": query.target_agent,
                }
            })),
        ),
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        ),
    }
}

fn parse_task_intent(intent: &str) -> Result<TaskIntent, String> {
    serde_json::from_str::<TaskIntent>(&format!("\"{intent}\""))
        .map_err(|_| format!("invalid intent '{intent}'"))
}

// ─── Chat Request/Response types ────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatRequest {
    /// The user's message.
    message: String,
    /// Optional session ID to continue a conversation.
    session_id: Option<String>,
    /// Optional hardware-bound device identifier for per-message verification.
    #[serde(default)]
    device_id: Option<String>,
    /// Optional base64/base64url device signature for `message`.
    #[serde(default)]
    device_signature: Option<String>,
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

fn default_web_handle(caller: &CallerIdentity) -> Option<String> {
    caller
        .username
        .clone()
        .or_else(|| caller.email.clone())
        .filter(|value| !value.trim().is_empty())
}

fn parse_session_id(value: &str) -> Option<SessionId> {
    uuid::Uuid::parse_str(value).ok().map(SessionId)
}

fn parse_channel_kind(value: &str) -> Result<ChannelKind, String> {
    serde_json::from_str::<ChannelKind>(&format!("\"{value}\""))
        .map_err(|_| format!("invalid channel '{value}'"))
}

fn channel_kind_label(value: &ChannelKind) -> String {
    match serde_json::to_string(value) {
        Ok(serialized) => serialized.trim_matches('"').to_string(),
        Err(_) => format!("{:?}", value),
    }
}

fn parse_user_role(value: &str) -> Result<UserRole, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "owner" => Ok(UserRole::Owner),
        "family" => Ok(UserRole::Family),
        "guest" => Ok(UserRole::Guest),
        other => Err(format!("invalid role '{other}'")),
    }
}

fn parse_attestation_type(value: &str) -> Result<AttestationType, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "tpm" => Ok(AttestationType::Tpm),
        "secureenclave" | "secure_enclave" | "secure-enclave" => Ok(AttestationType::SecureEnclave),
        "strongbox" | "strong_box" | "strong-box" => Ok(AttestationType::StrongBox),
        "clientcertificate" | "client_certificate" | "client-certificate" => {
            Ok(AttestationType::ClientCertificate)
        }
        "compositefingerprint" | "composite_fingerprint" | "composite-fingerprint" => {
            Ok(AttestationType::CompositeFingerprint)
        }
        other => Err(format!("invalid attestation '{other}'")),
    }
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
    /// Operator-facing orchestration diagnostics.
    orchestration: OrchestrationDiagnostics,
    /// Runtime identity diagnostics for this request.
    identity: IdentityResolutionDiagnostics,
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

    let max_len = state.config().gateway.max_message_length;
    if max_len > 0 && body.message.len() > max_len {
        return Json(json!({
            "error": format!(
                "message too long: {} chars (limit: {})",
                body.message.len(),
                max_len,
            )
        }));
    }

    let device_claim = runtime_identity::build_web_device_claim(
        body.device_id.as_deref(),
        body.device_signature.as_deref(),
        &body.message,
    );
    let resolved_identity = runtime_identity::resolve_web_identity_with_device(
        state.identity(),
        &caller,
        device_claim.as_ref(),
    );
    let identity =
        runtime_identity::describe_web_identity(&caller, &resolved_identity, device_claim.as_ref());
    if matches!(
        resolved_identity.action,
        IdentityAction::Challenge | IdentityAction::Block
    ) {
        return Json(json!({
            "error": "device verification failed for this request",
            "identity": identity,
        }));
    }
    let user_id = resolved_identity.user_id;

    // ── Session ──
    let session_id = if let Some(ref sid_str) = body.session_id {
        match uuid::Uuid::parse_str(sid_str) {
            Ok(uuid) => {
                let sid = ngenorca_core::SessionId(uuid);
                // Verify session exists, otherwise create new
                if state.sessions().get(&sid).is_some() {
                    if let Some(ref user_id) = user_id {
                        match state.sessions().promote_to_user(&sid, user_id) {
                            Ok(promoted) => promoted,
                            Err(e) => return Json(json!({ "error": e.to_string() })),
                        }
                    } else {
                        sid
                    }
                } else {
                    match state
                        .sessions()
                        .get_or_create(user_id.as_ref(), &body.channel)
                    {
                        Ok(sid) => sid,
                        Err(e) => return Json(json!({ "error": e.to_string() })),
                    }
                }
            }
            Err(_) => {
                match state
                    .sessions()
                    .get_or_create(user_id.as_ref(), &body.channel)
                {
                    Ok(sid) => sid,
                    Err(e) => return Json(json!({ "error": e.to_string() })),
                }
            }
        }
    } else {
        match state
            .sessions()
            .get_or_create(user_id.as_ref(), &body.channel)
        {
            Ok(sid) => sid,
            Err(e) => return Json(json!({ "error": e.to_string() })),
        }
    };

    // ── Build conversation context ──
    let orch = HybridOrchestrator::new(Arc::new(state.config().clone()));
    let classification = match orch.classify(&body.message, Some(state.providers())).await {
        Ok(classification) => classification,
        Err(e) => return Json(json!({ "error": e.to_string() })),
    };

    let mut conversation = body.conversation.clone();

    // Load memory context if available.
    let memory_ctx = if state.config().memory.enabled {
        if let Some(ref uid) = user_id {
            let token_budget = state.config().memory.semantic_token_budget;
            match state.memory().build_context_for_task(
                uid,
                &session_id,
                &body.message,
                &classification,
                token_budget,
            ) {
                Ok(ctx) => {
                    for wm in &ctx.working_messages {
                        conversation.push(ChatMessage {
                            role: wm.role.clone(),
                            content: wm.content.clone(),
                        });
                    }
                    Some(ctx)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to build memory context");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

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

    match orch
        .process_with_classification(
            &body.message,
            &classification,
            &conversation,
            state.providers(),
            Some(state.plugins()),
            memory_ctx.as_ref(),
            InvocationContext {
                learned_router: Some(state.learned_router()),
                session_id: Some(&session_id),
                user_id: user_id.as_ref(),
                channel: Some("web"),
                event_bus: Some(state.event_bus()),
            },
        )
        .await
    {
        Ok((response, record)) => {
            state.metrics().inc_orchestrations();
            state
                .metrics()
                .add_tokens(response.total_usage.total_tokens as u64);
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
                served_by: format!("{}/{}", response.served_by.name, response.served_by.model),
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
                orchestration: response.diagnostics,
                identity,
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
async fn list_sessions(State(state): State<AppState>) -> Json<serde_json::Value> {
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

// ─── Webhook Handler (SEC-05) ───────────────────────────────────

/// Generic inbound webhook handler for channel adapters.
///
/// Receives POST requests from external platforms (WhatsApp Cloud API, Slack
/// Events API, Telegram webhook mode, Teams Bot Framework), verifies signatures
/// where configured, and dispatches the payload to the appropriate adapter.
///
/// `POST /webhooks/{channel}`
async fn webhook_inbound(
    State(state): State<AppState>,
    axum::extract::Path(channel): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    match channel.as_str() {
        "whatsapp" => handle_whatsapp_webhook(&state, &headers, &body).await,
        "slack" => handle_slack_webhook(&state, &headers, &body).await,
        "telegram" => handle_telegram_webhook(&state, &headers, &body).await,
        "teams" => handle_teams_webhook(&state, &headers, &body).await,
        _ => (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "Unknown webhook channel" })),
        ),
    }
}

async fn handle_whatsapp_webhook(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let ch = &state.config().channels;
    let wa = match ch.whatsapp.as_ref().filter(|c| c.enabled) {
        Some(c) => c,
        None => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({ "error": "WhatsApp not enabled" })),
            );
        }
    };

    // SEC-05: Verify webhook signature when app_secret is configured.
    if wa.app_secret.is_some() {
        let sig = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // Build a temporary adapter to use verify_webhook_signature.
        let adapter = crate::channels::whatsapp::WhatsAppAdapter::cloud_api(
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            wa.app_secret.clone(),
        );

        if !adapter.verify_webhook_signature(body, sig) {
            warn!("WhatsApp webhook: invalid signature — rejecting");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Invalid webhook signature" })),
            );
        }
    }

    // Signature valid (or no secret configured) — parse payload and publish to EventBus.
    let messages = crate::channels::whatsapp::WhatsAppAdapter::parse_webhook_messages(body);
    for ngen_msg in messages {
        let event = ngenorca_core::event::Event {
            id: ngenorca_core::types::EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(ngen_msg.session_id.clone()),
            user_id: ngen_msg.user_id.clone(),
            payload: ngenorca_core::event::EventPayload::Message(ngen_msg),
        };
        if let Err(e) = state.event_bus().publish(event).await {
            warn!(error = %e, "WhatsApp webhook: failed to publish event");
        }
    }

    (axum::http::StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn handle_slack_webhook(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let ch = &state.config().channels;
    let sl = match ch.slack.as_ref().filter(|c| c.enabled) {
        Some(c) => c,
        None => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({ "error": "Slack not enabled" })),
            );
        }
    };

    // SEC-05: Verify Slack webhook signature when signing_secret is configured.
    if let Some(ref secret) = sl.signing_secret {
        let sig = headers
            .get("x-slack-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let ts = headers
            .get("x-slack-request-timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("0");

        if !crate::channels::slack::SlackAdapter::verify_webhook_signature(secret, ts, body, sig) {
            warn!("Slack webhook: invalid signature — rejecting");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Invalid webhook signature" })),
            );
        }
    }

    // Slack URL verification challenge.
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(body)
        && val.get("type").and_then(|v| v.as_str()) == Some("url_verification")
    {
        let challenge = val.get("challenge").and_then(|v| v.as_str()).unwrap_or("");
        return (
            axum::http::StatusCode::OK,
            Json(json!({ "challenge": challenge })),
        );
    }

    // Parse event_callback payload and publish messages to EventBus.
    let messages = crate::channels::slack::SlackAdapter::parse_webhook_messages(body);
    for ngen_msg in messages {
        let event = ngenorca_core::event::Event {
            id: ngenorca_core::types::EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(ngen_msg.session_id.clone()),
            user_id: ngen_msg.user_id.clone(),
            payload: ngenorca_core::event::EventPayload::Message(ngen_msg),
        };
        if let Err(e) = state.event_bus().publish(event).await {
            warn!(error = %e, "Slack webhook: failed to publish event");
        }
    }

    (axum::http::StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn handle_telegram_webhook(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    let ch = &state.config().channels;
    let tg = match ch.telegram.as_ref().filter(|c| c.enabled) {
        Some(c) => c,
        None => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({ "error": "Telegram not enabled" })),
            );
        }
    };

    // SEC-05: Verify Telegram webhook secret token (fail-closed).
    // The secret_token is set when calling setWebhook. We derive the expected
    // value from the bot token (first 32 chars of SHA-256 hex), so operators
    // don't need extra config.
    if let Some(ref bot_token) = tg.bot_token {
        let header_val = match headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|v| v.to_str().ok())
        {
            Some(v) => v,
            None => {
                warn!("Telegram webhook: missing secret token header — rejecting (fail-closed)");
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "Missing secret token" })),
                );
            }
        };

        use subtle::ConstantTimeEq;
        // Expected secret: first 32 hex chars of SHA-256(bot_token).
        use sha2::Digest;
        let hash = sha2::Sha256::digest(bot_token.as_bytes());
        let expected: String = hash.iter().take(16).map(|b| format!("{b:02x}")).collect();
        let a = header_val.as_bytes();
        let b = expected.as_bytes();
        if a.len() != b.len() || !bool::from(a.ct_eq(b)) {
            warn!("Telegram webhook: invalid secret token — rejecting");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Invalid secret token" })),
            );
        }
    }

    // Parse update and publish message to EventBus.
    let messages = crate::channels::telegram::TelegramAdapter::parse_webhook_messages(body);
    for ngen_msg in messages {
        let event = ngenorca_core::event::Event {
            id: ngenorca_core::types::EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(ngen_msg.session_id.clone()),
            user_id: ngen_msg.user_id.clone(),
            payload: ngenorca_core::event::EventPayload::Message(ngen_msg),
        };
        if let Err(e) = state.event_bus().publish(event).await {
            warn!(error = %e, "Telegram webhook: failed to publish event");
        }
    }

    (axum::http::StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn handle_teams_webhook(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    // SEC-05: Verify Bot Framework JWT (fail-closed — header is required).
    let auth = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some(v) => v,
        None => {
            warn!("Teams webhook: missing authorization header — rejecting (fail-closed)");
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Missing authorization" })),
            );
        }
    };

    // Extract the expected audience (app_id) from Teams channel config.
    let expected_audience = state
        .config()
        .channels
        .teams
        .as_ref()
        .and_then(|t| t.app_id.as_deref());

    // Full JWKS-based JWT verification (issuer, audience, expiry, signature).
    if !crate::channels::teams::TeamsAdapter::verify_bot_framework_jwt(auth, expected_audience)
        .await
    {
        warn!("Teams webhook: JWT verification failed — rejecting");
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid authorization" })),
        );
    }

    // Parse activity and publish message to EventBus.
    let messages = crate::channels::teams::TeamsAdapter::parse_webhook_messages(body);
    for ngen_msg in messages {
        let event = ngenorca_core::event::Event {
            id: ngenorca_core::types::EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(ngen_msg.session_id.clone()),
            user_id: ngen_msg.user_id.clone(),
            payload: ngenorca_core::event::EventPayload::Message(ngen_msg),
        };
        if let Err(e) = state.event_bus().publish(event).await {
            warn!(error = %e, "Teams webhook: failed to publish event");
        }
    }

    (axum::http::StatusCode::OK, Json(json!({ "status": "ok" })))
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
    /// Optional hardware-bound device identifier for this message.
    #[serde(default)]
    device_id: Option<String>,
    /// Optional base64/base64url device signature for this message.
    #[serde(default)]
    device_signature: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    orchestration: Option<OrchestrationDiagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<IdentityResolutionDiagnostics>,
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
    // OPS-03: Connection cap — reject if too many concurrent WS connections.
    const WS_MAX_CONNECTIONS: u64 = 256;
    if state.metrics().ws_connections_active() >= WS_MAX_CONNECTIONS {
        warn!("WebSocket connection rejected — max connections ({WS_MAX_CONNECTIONS}) reached");
        // Close immediately with policy violation code.
        let mut socket = socket;
        let _ = socket
            .send(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                code: 1008, // Policy Violation
                reason: "Max concurrent connections reached".into(),
            })))
            .await;
        return;
    }

    state.metrics().inc_ws_connections();
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to real-time events from the bus for push notifications.
    let mut event_rx = state.event_bus().subscribe();

    let resolved_identity = runtime_identity::resolve_web_identity(state.identity(), &caller);
    let connection_identity =
        runtime_identity::describe_web_identity(&caller, &resolved_identity, None);
    let user_id = resolved_identity.user_id;

    let display_user = caller.username.as_deref().unwrap_or("anonymous");
    info!(
        user = %display_user,
        "WebSocket connection established"
    );

    // OPS-03: Per-connection message rate limiting.
    // Simple token-bucket: max 30 messages per 60-second window.
    const WS_RATE_LIMIT_MAX: u32 = 30;
    const WS_RATE_LIMIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
    let mut ws_msg_count: u32 = 0;
    let mut ws_window_start = std::time::Instant::now();

    // Send a welcome message
    let welcome = WsChatResponse {
        msg_type: "connected".into(),
        content: Some("Connected to NgenOrca".into()),
        session_id: None,
        served_by: None,
        error: None,
        latency_ms: None,
        orchestration: None,
        identity: Some(connection_identity),
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

                // OPS-03: Per-connection rate limiting.
                if ws_window_start.elapsed() >= WS_RATE_LIMIT_WINDOW {
                    ws_msg_count = 0;
                    ws_window_start = std::time::Instant::now();
                }
                ws_msg_count += 1;
                if ws_msg_count > WS_RATE_LIMIT_MAX {
                    warn!(user = %display_user, "WebSocket rate limit exceeded");
                    let err_resp = WsChatResponse {
                        msg_type: "error".into(),
                        content: None,
                        session_id: None,
                        served_by: None,
                        error: Some("Rate limit exceeded — please slow down".into()),
                        latency_ms: None,
                        orchestration: None,
                        identity: None,
                    };
                    if let Ok(json) = serde_json::to_string(&err_resp) {
                        let _ = sender.send(WsMessage::Text(json.into())).await;
                    }
                    continue;
                }

                state.metrics().inc_ws_messages_in();
                if let Err(done) = handle_client_message(
                    &msg, &state, &mut sender, &caller, display_user,
                ).await
                    && done
                {
                    break;
                }
            }

            // Arm 2: Events pushed from the EventBus (scoped per-user)
            event = event_rx.recv() => {
                match event {
                    Ok(bus_event) => {
                        // SEC-02: Only forward events that belong to this user
                        // or are system-level (no user_id). Prevents cross-user
                        // event visibility on shared deployments.
                        if let Some(ref event_uid) = bus_event.user_id {
                            if let Some(ref my_uid) = user_id {
                                if event_uid != my_uid {
                                    continue; // Not our event — skip
                                }
                            }
                            // If WS connection is anonymous, still skip user-scoped events
                            // unless we explicitly want broadcast behavior.
                            else {
                                continue;
                            }
                        }
                        // Events with no user_id are system-level → broadcast to all.

                        let ws_event = WsEventPush {
                            msg_type: "event".into(),
                            event_id: bus_event.id.to_string(),
                            event_kind: event_payload_kind(&bus_event.payload),
                            session_id: bus_event.session_id.as_ref().map(|s| s.to_string()),
                            payload: serde_json::to_value(&bus_event.payload).ok(),
                            timestamp: bus_event.timestamp.to_rfc3339(),
                        };
                        if let Ok(json) = serde_json::to_string(&ws_event)
                            && sender.send(WsMessage::Text(json.into())).await.is_err()
                        {
                            break; // Client disconnected
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
    // OPS-03: Decrement active connection gauge on disconnect.
    state.metrics().dec_ws_connections();
}

/// Process a single inbound client message. Returns `Err(true)` if the
/// connection should be closed, `Err(false)` if the message was handled
/// (but the caller should `continue`), and `Ok(())` on success.
async fn handle_client_message(
    raw: &str,
    state: &AppState,
    sender: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    caller: &CallerIdentity,
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
                orchestration: None,
                identity: None,
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

    let device_claim = runtime_identity::build_web_device_claim(
        ws_msg.device_id.as_deref(),
        ws_msg.device_signature.as_deref(),
        &ws_msg.message,
    );
    let resolved_identity = runtime_identity::resolve_web_identity_with_device(
        state.identity(),
        caller,
        device_claim.as_ref(),
    );
    let identity =
        runtime_identity::describe_web_identity(caller, &resolved_identity, device_claim.as_ref());
    if matches!(
        resolved_identity.action,
        IdentityAction::Challenge | IdentityAction::Block
    ) {
        let err_resp = WsChatResponse {
            msg_type: "error".into(),
            content: None,
            session_id: None,
            served_by: None,
            error: Some("Device verification failed for this message".into()),
            latency_ms: None,
            orchestration: None,
            identity: Some(identity),
        };
        if let Ok(json) = serde_json::to_string(&err_resp) {
            let _ = sender.send(WsMessage::Text(json.into())).await;
        }
        return Err(false);
    }
    let user_id = resolved_identity.user_id.as_ref();

    // Enforce max message length
    let max_len = state.config().gateway.max_message_length;
    if max_len > 0 && ws_msg.message.len() > max_len {
        let err_resp = WsChatResponse {
            msg_type: "error".into(),
            content: None,
            session_id: None,
            served_by: None,
            error: Some(format!(
                "message too long: {} chars (limit: {})",
                ws_msg.message.len(),
                max_len,
            )),
            latency_ms: None,
            orchestration: None,
            identity: Some(identity.clone()),
        };
        if let Ok(json) = serde_json::to_string(&err_resp) {
            let _ = sender.send(WsMessage::Text(json.into())).await;
        }
        return Err(false);
    }

    // Get or create session. WebSocket uses the same logical webchat channel as HTTP.
    let session_id = if let Some(ref sid_str) = ws_msg.session_id {
        match uuid::Uuid::parse_str(sid_str) {
            Ok(uuid) => {
                let sid = ngenorca_core::SessionId(uuid);
                if state.sessions().get(&sid).is_some() {
                    if let Some(user_id) = user_id {
                        match state.sessions().promote_to_user(&sid, user_id) {
                            Ok(promoted) => promoted,
                            Err(e) => {
                                let err_resp = WsChatResponse {
                                    msg_type: "error".into(),
                                    content: None,
                                    session_id: None,
                                    served_by: None,
                                    error: Some(format!("Session error: {e}")),
                                    latency_ms: None,
                                    orchestration: None,
                                    identity: Some(identity.clone()),
                                };
                                if let Ok(json) = serde_json::to_string(&err_resp) {
                                    let _ = sender.send(WsMessage::Text(json.into())).await;
                                }
                                return Err(false);
                            }
                        }
                    } else {
                        sid
                    }
                } else {
                    match state.sessions().get_or_create(user_id, "webchat") {
                        Ok(sid) => sid,
                        Err(e) => {
                            let err_resp = WsChatResponse {
                                msg_type: "error".into(),
                                content: None,
                                session_id: None,
                                served_by: None,
                                error: Some(format!("Session error: {e}")),
                                latency_ms: None,
                                orchestration: None,
                                identity: Some(identity.clone()),
                            };
                            if let Ok(json) = serde_json::to_string(&err_resp) {
                                let _ = sender.send(WsMessage::Text(json.into())).await;
                            }
                            return Err(false);
                        }
                    }
                }
            }
            Err(_) => match state.sessions().get_or_create(user_id, "webchat") {
                Ok(sid) => sid,
                Err(e) => {
                    let err_resp = WsChatResponse {
                        msg_type: "error".into(),
                        content: None,
                        session_id: None,
                        served_by: None,
                        error: Some(format!("Session error: {e}")),
                        latency_ms: None,
                        orchestration: None,
                        identity: Some(identity.clone()),
                    };
                    if let Ok(json) = serde_json::to_string(&err_resp) {
                        let _ = sender.send(WsMessage::Text(json.into())).await;
                    }
                    return Err(false);
                }
            },
        }
    } else {
        match state.sessions().get_or_create(user_id, "webchat") {
            Ok(sid) => sid,
            Err(e) => {
                let err_resp = WsChatResponse {
                    msg_type: "error".into(),
                    content: None,
                    session_id: None,
                    served_by: None,
                    error: Some(format!("Session error: {e}")),
                    latency_ms: None,
                    orchestration: None,
                    identity: Some(identity.clone()),
                };
                if let Ok(json) = serde_json::to_string(&err_resp) {
                    let _ = sender.send(WsMessage::Text(json.into())).await;
                }
                return Err(false);
            }
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
        orchestration: None,
        identity: Some(identity.clone()),
    };
    if let Ok(json) = serde_json::to_string(&thinking) {
        let _ = sender.send(WsMessage::Text(json.into())).await;
    }

    let orch = HybridOrchestrator::new(Arc::new(state.config().clone()));
    let classification = match orch
        .classify(&ws_msg.message, Some(state.providers()))
        .await
    {
        Ok(classification) => classification,
        Err(e) => {
            let err_resp = WsChatResponse {
                msg_type: "error".into(),
                content: None,
                session_id: Some(session_id.to_string()),
                served_by: None,
                error: Some(format!("Classification error: {e}")),
                latency_ms: None,
                orchestration: None,
                identity: Some(identity.clone()),
            };
            if let Ok(json) = serde_json::to_string(&err_resp) {
                let _ = sender.send(WsMessage::Text(json.into())).await;
            }
            return Err(false);
        }
    };

    // Load memory context before adding the current message to working memory,
    // so retrieval uses prior session history and cross-session recall.
    let memory_ctx = if state.config().memory.enabled {
        if let Some(uid) = user_id {
            let token_budget = state.config().memory.semantic_token_budget;
            match state.memory().build_context_for_task(
                uid,
                &session_id,
                &ws_msg.message,
                &classification,
                token_budget,
            ) {
                Ok(ctx) => Some(ctx),
                Err(e) => {
                    warn!(error = %e, "Failed to build WebSocket memory context");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

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

    // Build conversation from prior working memory.
    let conversation: Vec<ChatMessage> = memory_ctx
        .as_ref()
        .map(|ctx| {
            ctx.working_messages
                .iter()
                .map(|wm| ChatMessage {
                    role: wm.role.clone(),
                    content: wm.content.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    let response = match orch
        .process_with_classification(
            &ws_msg.message,
            &classification,
            &conversation,
            state.providers(),
            Some(state.plugins()),
            memory_ctx.as_ref(),
            InvocationContext {
                learned_router: Some(state.learned_router()),
                session_id: Some(&session_id),
                user_id,
                channel: Some("websocket"),
                event_bus: Some(state.event_bus()),
            },
        )
        .await
    {
        Ok((resp, record)) => {
            state.metrics().inc_orchestrations();
            state
                .metrics()
                .add_tokens(resp.total_usage.total_tokens as u64);
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
                user_id: user_id.cloned(),
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

            if let Some(uid) = user_id {
                let entry = ngenorca_memory::episodic::EpisodicEntry {
                    id: 0,
                    user_id: uid.0.clone(),
                    content: format!("User: {}\nAssistant: {}", ws_msg.message, resp.content),
                    summary: None,
                    channel: "webchat".into(),
                    timestamp: chrono::Utc::now(),
                    embedding: None,
                    relevance_score: 0.0,
                };
                if let Err(e) = state.memory().episodic.store(&entry) {
                    warn!(error = %e, "Failed to store WS episodic memory");
                }
            }

            WsChatResponse {
                msg_type: "response".into(),
                content: Some(resp.content),
                session_id: Some(session_id.to_string()),
                served_by: Some(format!("{}/{}", resp.served_by.name, resp.served_by.model)),
                error: None,
                latency_ms: Some(resp.latency_ms),
                orchestration: Some(resp.diagnostics),
                identity: Some(identity.clone()),
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
                orchestration: None,
                identity: Some(identity),
            }
        }
    };

    if let Ok(json) = serde_json::to_string(&response)
        && sender.send(WsMessage::Text(json.into())).await.is_err()
    {
        return Err(true); // Client disconnected
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
