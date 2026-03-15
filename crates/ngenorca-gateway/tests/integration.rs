//! HTTP integration tests for the NgenOrca gateway.
//!
//! These tests build the full middleware stack (auth, rate-limit, request-ID)
//! and exercise routes via `tower::ServiceExt::oneshot`, without binding to a
//! real TCP port.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware;
use base64::Engine;
use ngenorca_bus::EventBus;
use ngenorca_config::NgenOrcaConfig;
use ngenorca_core::event::{Event, EventPayload};
use ngenorca_core::identity::{AttestationType, UserRole};
use ngenorca_core::orchestration::{
    ClassificationMethod, CorrectionRecord, OrchestrationRecord, QualityMethod, QualityVerdict,
    RoutingDecision, SubAgentId, SynthesisRecord, TaskClassification, TaskComplexity, TaskIntent,
};
use ngenorca_core::types::{DeviceId, EventId, SessionId, TrustLevel, UserId};
use ngenorca_gateway::auth;
use ngenorca_gateway::metrics::Metrics;
use ngenorca_gateway::orchestration::LearnedRouter;
use ngenorca_gateway::plugins::PluginRegistry;
use ngenorca_gateway::providers::ProviderRegistry;
use ngenorca_gateway::rate_limit::RateLimiterState;
use ngenorca_gateway::routes;
use ngenorca_gateway::sessions::SessionManager;
use ngenorca_gateway::state::{AppState, AppStateParams};
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
    let temp_dir =
        std::env::temp_dir().join(format!("ngenorca_integ_{}_{unique}", std::process::id(),));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let config_file_path = temp_dir.join("config.toml");
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
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

fn learned_record(
    intent: TaskIntent,
    target: &str,
    domain_tags: Vec<String>,
) -> OrchestrationRecord {
    let classification = TaskClassification {
        intent,
        complexity: TaskComplexity::Simple,
        confidence: 0.95,
        method: ClassificationMethod::RuleBased,
        domain_tags,
        language: Some("en".into()),
    };
    let routing = RoutingDecision {
        target: SubAgentId {
            name: target.into(),
            model: "test-model".into(),
        },
        reason: "test learned route".into(),
        system_prompt: String::new(),
        temperature: Some(0.1),
        max_tokens: Some(256),
        from_memory: false,
    };

    OrchestrationRecord {
        classification,
        routing,
        quality: QualityVerdict::Accept { score: Some(0.9) },
        quality_method: QualityMethod::Heuristic,
        escalated: false,
        user_id: Some(UserId("ops".into())),
        channel: Some("web".into()),
        latency_ms: 10,
        total_tokens: 42,
        correction: CorrectionRecord {
            tool_rounds: 1,
            had_failures: false,
            had_blocked_calls: false,
            verification_attempted: true,
            grounded: true,
            remediation_attempted: false,
            remediation_succeeded: false,
            post_synthesis_verification_attempted: false,
            post_synthesis_drift_corrected: false,
        },
        synthesis: SynthesisRecord {
            attempted: true,
            succeeded: true,
            contradiction_score: 0.0,
            conflicting_branches: 0,
        },
        timestamp: chrono::Utc::now(),
    }
}

async fn seed_operator_history_events(state: &AppState) {
    let mut accepted = learned_record(TaskIntent::Coding, "primary", vec!["rust".into()]);
    accepted.timestamp = chrono::Utc::now() - chrono::Duration::hours(50);

    let mut escalated = learned_record(TaskIntent::Analysis, "deep-thinker", vec!["logs".into()]);
    escalated.timestamp = chrono::Utc::now() - chrono::Duration::hours(10);
    escalated.quality = QualityVerdict::Escalate {
        reason: "needs deeper review".into(),
        escalate_to: Some("deep-thinker".into()),
    };
    escalated.escalated = true;
    escalated.channel = Some("web".into());
    escalated.correction.had_failures = true;
    escalated.correction.verification_attempted = true;
    escalated.correction.remediation_attempted = true;
    escalated.correction.remediation_succeeded = true;
    escalated.correction.post_synthesis_drift_corrected = true;

    state
        .event_bus()
        .publish(Event {
            id: EventId::new(),
            timestamp: accepted.timestamp,
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("ops".into())),
            payload: EventPayload::OrchestrationCompleted(accepted),
        })
        .await
        .unwrap();
    state
        .event_bus()
        .publish(Event {
            id: EventId::new(),
            timestamp: escalated.timestamp,
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("ops".into())),
            payload: EventPayload::OrchestrationCompleted(escalated),
        })
        .await
        .unwrap();
    state
        .event_bus()
        .publish(Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(10),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("ops".into())),
            payload: EventPayload::ToolExecution {
                tool_name: "run_command".into(),
                session_id: SessionId::new(),
                channel: Some("web".into()),
                started_at: chrono::Utc::now() - chrono::Duration::hours(10),
                duration_ms: Some(12),
                success: Some(false),
                failure_class: Some("execution".into()),
                outcome: Some("failed".into()),
            },
        })
        .await
        .unwrap();
    state
        .event_bus()
        .publish(Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now() - chrono::Duration::hours(2),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("other".into())),
            payload: EventPayload::ToolExecution {
                tool_name: "read_file".into(),
                session_id: SessionId::new(),
                channel: Some("cli".into()),
                started_at: chrono::Utc::now() - chrono::Duration::hours(2),
                duration_ms: Some(4),
                success: Some(true),
                failure_class: None,
                outcome: Some("success".into()),
            },
        })
        .await
        .unwrap();
}

async fn state_with_identity(config: NgenOrcaConfig, identity: IdentityManager) -> AppState {
    let event_bus = EventBus::new(":memory:").await.unwrap();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "ngenorca_integ_identity_{}_{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    AppState::new(AppStateParams {
        config,
        config_file_path: temp_dir.join("config.toml"),
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
async fn whoami_reports_runtime_identity_guidance() {
    let app = build_app(test_state().await);
    let req = Request::builder()
        .uri("/api/v1/whoami")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["identity"]["action"], "require_pairing");
    assert_eq!(json["identity"]["requires_pairing"], true);
    assert!(
        json["identity"]["suggested_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str().unwrap().contains("Pair a device"))
    );
}

#[tokio::test]
async fn chat_reports_structured_identity_challenge_details() {
    let config = NgenOrcaConfig::default();
    let identity = IdentityManager::new(":memory:").unwrap();
    let canonical = UserId("owner-alice".into());
    identity
        .register_user(canonical.clone(), "Alice".into(), UserRole::Owner)
        .unwrap();
    identity
        .pair_device(
            &canonical,
            ngenorca_core::identity::DeviceBinding {
                device_id: DeviceId("dev-webchat".into()),
                device_name: "Primary laptop".into(),
                attestation: AttestationType::CompositeFingerprint,
                public_key_hash: base64::engine::general_purpose::STANDARD.encode([0u8; 32]),
                trust: TrustLevel::Hardware,
                paired_at: chrono::Utc::now(),
                last_used: chrono::Utc::now(),
            },
        )
        .unwrap();

    let app = build_app(state_with_identity(config, identity).await);
    let body = serde_json::json!({
        "message": "hello",
        "device_id": "dev-webchat",
        "device_signature": base64::engine::general_purpose::STANDARD.encode([0u8; 64])
    });
    let req = Request::builder()
        .uri("/api/v1/chat")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("device verification failed")
    );
    assert_eq!(json["identity"]["action"], "challenge");
    assert_eq!(json["identity"]["requires_challenge"], true);
    assert_eq!(json["identity"]["device_id"], "dev-webchat");
    assert!(
        json["identity"]["suggested_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str().unwrap().contains("fresh signed payload"))
    );
}

#[tokio::test]
async fn identity_pairing_endpoints_can_bind_user_and_session() {
    let state = test_state().await;
    let session_id = state.sessions().get_or_create(None, "webchat").unwrap();
    let app = build_app(state.clone());

    let start_req = Request::builder()
        .uri("/api/v1/identity/pairing/start")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "handle": "alice",
                "channel": "WebChat"
            }))
            .unwrap(),
        ))
        .unwrap();
    let start_resp = app.clone().oneshot(start_req).await.unwrap();
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body = axum::body::to_bytes(start_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let start_json: Value = serde_json::from_slice(&start_body).unwrap();
    let pairing_id = start_json["pairing_id"].as_str().unwrap();

    let complete_req = Request::builder()
        .uri("/api/v1/identity/pairing/complete")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "pairing_id": pairing_id,
                "user_id": "alice",
                "display_name": "Alice",
                "role": "owner",
                "session_id": session_id.to_string(),
                "channel_id": "webchat:alice"
            }))
            .unwrap(),
        ))
        .unwrap();
    let complete_resp = app.oneshot(complete_req).await.unwrap();
    assert_eq!(complete_resp.status(), StatusCode::OK);
    let complete_body = axum::body::to_bytes(complete_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let complete_json: Value = serde_json::from_slice(&complete_body).unwrap();
    assert_eq!(complete_json["paired"], true);
    assert_eq!(complete_json["user"]["user_id"], "alice");
    assert_eq!(complete_json["session_id"], session_id.to_string());
    assert_eq!(
        state.sessions().get(&session_id).unwrap().user_id,
        Some(UserId("alice".into()))
    );
}

#[tokio::test]
async fn identity_pairing_completion_reuses_existing_canonical_session() {
    let state = test_state().await;
    state
        .identity()
        .register_user(UserId("alice".into()), "Alice".into(), UserRole::Owner)
        .unwrap();
    let canonical_session = state
        .sessions()
        .get_or_create(Some(&UserId("alice".into())), "webchat")
        .unwrap();
    let anonymous_session = state.sessions().get_or_create(None, "webchat").unwrap();
    let app = build_app(state.clone());

    let start_req = Request::builder()
        .uri("/api/v1/identity/pairing/start")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "handle": "alice",
                "channel": "WebChat"
            }))
            .unwrap(),
        ))
        .unwrap();
    let start_resp = app.clone().oneshot(start_req).await.unwrap();
    let start_body = axum::body::to_bytes(start_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let start_json: Value = serde_json::from_slice(&start_body).unwrap();
    let pairing_id = start_json["pairing_id"].as_str().unwrap();

    let complete_req = Request::builder()
        .uri("/api/v1/identity/pairing/complete")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "pairing_id": pairing_id,
                "user_id": "alice",
                "display_name": "Alice",
                "role": "owner",
                "session_id": anonymous_session.to_string(),
                "channel_id": "webchat:alice"
            }))
            .unwrap(),
        ))
        .unwrap();
    let complete_resp = app.oneshot(complete_req).await.unwrap();
    assert_eq!(complete_resp.status(), StatusCode::OK);
    let complete_body = axum::body::to_bytes(complete_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let complete_json: Value = serde_json::from_slice(&complete_body).unwrap();

    assert_eq!(complete_json["session_id"], canonical_session.to_string());
    assert_eq!(
        state.sessions().get(&anonymous_session).unwrap().state,
        ngenorca_core::session::SessionState::Ended
    );
}

#[tokio::test]
async fn identity_challenge_endpoints_verify_known_device_and_rebind_session() {
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    let config = NgenOrcaConfig::default();
    let identity = IdentityManager::new(":memory:").unwrap();
    let canonical = UserId("owner-alice".into());
    identity
        .register_user(canonical.clone(), "Alice".into(), UserRole::Owner)
        .unwrap();

    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    identity
        .pair_device(
            &canonical,
            ngenorca_core::identity::DeviceBinding {
                device_id: DeviceId("dev-webchat".into()),
                device_name: "Primary laptop".into(),
                attestation: AttestationType::CompositeFingerprint,
                public_key_hash: base64::engine::general_purpose::STANDARD
                    .encode(key_pair.public_key().as_ref()),
                trust: TrustLevel::Hardware,
                paired_at: chrono::Utc::now(),
                last_used: chrono::Utc::now(),
            },
        )
        .unwrap();

    let state = state_with_identity(config, identity).await;
    let session_id = state.sessions().get_or_create(None, "webchat").unwrap();
    let app = build_app(state.clone());

    let start_req = Request::builder()
        .uri("/api/v1/identity/challenge/start")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "device_id": "dev-webchat",
                "reason": "retry after signature failure"
            }))
            .unwrap(),
        ))
        .unwrap();
    let start_resp = app.clone().oneshot(start_req).await.unwrap();
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body = axum::body::to_bytes(start_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let start_json: Value = serde_json::from_slice(&start_body).unwrap();
    let challenge_id = start_json["challenge_id"].as_str().unwrap();
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(start_json["nonce_b64"].as_str().unwrap())
        .unwrap();
    let signature =
        base64::engine::general_purpose::STANDARD.encode(key_pair.sign(&nonce).as_ref());

    let verify_req = Request::builder()
        .uri("/api/v1/identity/challenge/verify")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "challenge_id": challenge_id,
                "signature": signature,
                "session_id": session_id.to_string()
            }))
            .unwrap(),
        ))
        .unwrap();
    let verify_resp = app.oneshot(verify_req).await.unwrap();
    assert_eq!(verify_resp.status(), StatusCode::OK);
    let verify_body = axum::body::to_bytes(verify_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let verify_json: Value = serde_json::from_slice(&verify_body).unwrap();
    assert_eq!(verify_json["verified"], true);
    assert_eq!(verify_json["user_id"], canonical.0);
    assert_eq!(verify_json["session_id"], session_id.to_string());
    assert_eq!(
        state.sessions().get(&session_id).unwrap().user_id,
        Some(canonical)
    );
}

#[tokio::test]
async fn identity_challenge_verify_reuses_existing_canonical_session_and_links_web_handle() {
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::TrustedProxy;
    config.gateway.trusted_proxy_sources.clear();

    let identity = IdentityManager::new(":memory:").unwrap();
    let canonical = UserId("owner-alice".into());
    identity
        .register_user(canonical.clone(), "Alice".into(), UserRole::Owner)
        .unwrap();

    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    identity
        .pair_device(
            &canonical,
            ngenorca_core::identity::DeviceBinding {
                device_id: DeviceId("dev-webchat".into()),
                device_name: "Primary laptop".into(),
                attestation: AttestationType::CompositeFingerprint,
                public_key_hash: base64::engine::general_purpose::STANDARD
                    .encode(key_pair.public_key().as_ref()),
                trust: TrustLevel::Hardware,
                paired_at: chrono::Utc::now(),
                last_used: chrono::Utc::now(),
            },
        )
        .unwrap();

    let state = state_with_identity(config, identity).await;
    let canonical_session = state
        .sessions()
        .get_or_create(Some(&canonical), "webchat")
        .unwrap();
    let anonymous_session = state.sessions().get_or_create(None, "webchat").unwrap();
    let app = build_app(state.clone());

    let start_req = Request::builder()
        .uri("/api/v1/identity/challenge/start")
        .method("POST")
        .header("content-type", "application/json")
        .header("remote-user", "alice-proxy")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "device_id": "dev-webchat",
                "reason": "retry after signature failure"
            }))
            .unwrap(),
        ))
        .unwrap();
    let start_resp = app.clone().oneshot(start_req).await.unwrap();
    assert_eq!(start_resp.status(), StatusCode::OK);
    let start_body = axum::body::to_bytes(start_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let start_json: Value = serde_json::from_slice(&start_body).unwrap();
    let challenge_id = start_json["challenge_id"].as_str().unwrap();
    let nonce = base64::engine::general_purpose::STANDARD
        .decode(start_json["nonce_b64"].as_str().unwrap())
        .unwrap();
    let signature =
        base64::engine::general_purpose::STANDARD.encode(key_pair.sign(&nonce).as_ref());

    let verify_req = Request::builder()
        .uri("/api/v1/identity/challenge/verify")
        .method("POST")
        .header("content-type", "application/json")
        .header("remote-user", "alice-proxy")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "challenge_id": challenge_id,
                "signature": signature,
                "session_id": anonymous_session.to_string()
            }))
            .unwrap(),
        ))
        .unwrap();
    let verify_resp = app.oneshot(verify_req).await.unwrap();
    assert_eq!(verify_resp.status(), StatusCode::OK);
    let verify_body = axum::body::to_bytes(verify_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let verify_json: Value = serde_json::from_slice(&verify_body).unwrap();

    assert_eq!(verify_json["session_id"], canonical_session.to_string());
    assert_eq!(verify_json["linked_handles"][0], "alice-proxy");
    assert_eq!(
        state
            .identity()
            .resolve_by_channel(&ngenorca_core::types::ChannelKind::WebChat, "alice-proxy")
            .unwrap()
            .unwrap()
            .0,
        canonical
    );
    assert_eq!(
        state.sessions().get(&anonymous_session).unwrap().state,
        ngenorca_core::session::SessionState::Ended
    );
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

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "healthy");
    assert!(json["sandbox"]["environment"].is_string());
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

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert!(json["version"].is_string());
    // Must not be empty.
    assert!(!json["version"].as_str().unwrap().is_empty());
}

// ─── Root endpoint ──────────────────────────────────────────────────

#[tokio::test]
async fn root_returns_name_and_endpoints() {
    let app = build_app(test_state().await);
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "NgenOrca");
    assert!(json["endpoints"].is_object());
}

#[tokio::test]
async fn config_page_returns_html() {
    let app = build_app(test_state().await);
    let req = Request::builder()
        .uri("/config")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"))
    );
}

#[tokio::test]
async fn config_file_endpoint_reads_and_writes_persisted_config() {
    let config = NgenOrcaConfig::default();
    let event_bus = EventBus::new(":memory:").await.unwrap();
    let identity = IdentityManager::new(":memory:").unwrap();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "ngenorca_integ_config_ui_{}_{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let config_file_path = temp_dir.join("config.toml");
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    let state = AppState::new(AppStateParams {
        config,
        config_file_path: config_file_path.clone(),
        event_bus,
        identity,
        memory,
        providers,
        sessions,
        plugins,
        learned_router,
        metrics,
    });

    let app = build_app(state.clone());

    let load_req = Request::builder()
        .uri("/api/v1/config/file")
        .body(Body::empty())
        .unwrap();
    let load_resp = app.clone().oneshot(load_req).await.unwrap();
    assert_eq!(load_resp.status(), StatusCode::OK);
    let load_body = axum::body::to_bytes(load_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let load_json: Value = serde_json::from_slice(&load_body).unwrap();
    assert_eq!(load_json["exists"], false);

    let new_config = r#"
[gateway]
auth_mode = "None"

[agent]
model = "anthropic/claude-sonnet-4-20250514"

[channels.webchat]
enabled = true
"#;

    let save_req = Request::builder()
        .method("PUT")
        .uri("/api/v1/config/file")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "content": new_config }).to_string(),
        ))
        .unwrap();
    let save_resp = app.oneshot(save_req).await.unwrap();
    assert_eq!(save_resp.status(), StatusCode::OK);

    let persisted = std::fs::read_to_string(&config_file_path).unwrap();
    assert!(persisted.contains("[agent]"));
    assert!(persisted.contains("anthropic/claude-sonnet-4-20250514"));
}

#[tokio::test]
async fn orchestration_endpoint_reports_learned_route_diagnostics() {
    let state = test_state().await;
    state
        .learned_router()
        .ingest(&learned_record(TaskIntent::Coding, "primary", vec![]))
        .unwrap();
    let mut escalated = learned_record(TaskIntent::Analysis, "deep-thinker", vec!["logs".into()]);
    escalated.quality = QualityVerdict::Escalate {
        reason: "needs deeper review".into(),
        escalate_to: Some("deep-thinker".into()),
    };
    escalated.escalated = true;
    escalated.latency_ms = 50;
    escalated.total_tokens = 120;
    state
        .event_bus()
        .publish(Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("ops".into())),
            payload: EventPayload::OrchestrationCompleted(learned_record(
                TaskIntent::Coding,
                "primary",
                vec!["rust".into()],
            )),
        })
        .await
        .unwrap();
    state
        .event_bus()
        .publish(Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("ops".into())),
            payload: EventPayload::OrchestrationCompleted(escalated),
        })
        .await
        .unwrap();
    state
        .event_bus()
        .publish(Event {
            id: EventId::new(),
            timestamp: chrono::Utc::now(),
            session_id: Some(SessionId::new()),
            user_id: Some(UserId("ops".into())),
            payload: EventPayload::ToolExecution {
                tool_name: "run_command".into(),
                session_id: SessionId::new(),
                channel: Some("web".into()),
                started_at: chrono::Utc::now(),
                duration_ms: Some(12),
                success: Some(false),
                failure_class: Some("execution".into()),
                outcome: Some("failed".into()),
            },
        })
        .await
        .unwrap();

    let app = build_app(state);
    let req = Request::builder()
        .uri("/api/v1/orchestration")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["learned_routes"]["count"], 1);
    assert_eq!(
        json["learned_routes"]["rules"][0]["rule"]["target_agent"],
        "primary"
    );
    assert!(json["learned_routes"]["rules"][0]["effective_confidence"].is_number());
    assert!(json["learned_routes"]["rules"][0]["adaptive_decay_multiplier"].is_number());
    assert!(json["learned_routes"]["rules"][0]["outcome_trend_adjustment"].is_number());
    assert!(json["learned_routes"]["rules"][0]["accept_rate"].is_number());
    assert!(json["learned_routes"]["rules"][0]["stability_score"].is_number());
    assert_eq!(json["learned_routes"]["history"]["total_rules"], 1);
    assert!(
        json["learned_routes"]["history"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["label"] == "primary")
    );
    assert_eq!(
        json["execution_diagnostics"]["response_metadata_exposed"],
        true
    );
    assert_eq!(
        json["execution_diagnostics"]["tracks_structured_planning"],
        true
    );
    assert_eq!(
        json["execution_diagnostics"]["tracks_tool_verification"],
        true
    );
    assert_eq!(
        json["execution_diagnostics"]["tracks_branch_contradiction_analysis"],
        true
    );
    assert_eq!(
        json["execution_diagnostics"]["tracks_learned_route_trends"],
        true
    );
    assert!(
        json["execution_diagnostics"]["worker_stage_reporting"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "parallel-support")
    );
    assert!(
        json["execution_diagnostics"]["worker_stage_reporting"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "escalation")
    );
    assert_eq!(json["recent_history"]["orchestration_count"], 2);
    assert_eq!(json["recent_history"]["tool_event_count"], 1);
    assert!(json["recent_history"]["escalation_rate"].as_f64().unwrap() > 0.0);
    assert!(
        json["recent_history"]["tool_failure_rate"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(
        json["recent_history"]["grounded_response_rate"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(
        json["recent_history"]["correction_attempt_rate"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(
        json["recent_history"]["intent_mix"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["label"] == "Coding")
    );
    assert!(
        json["recent_history"]["user_mix"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["label"] == "ops")
    );
    assert!(
        json["recent_history"]["channel_mix"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["label"] == "web")
    );
    assert!(
        json["recent_history"]["tool_mix"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["label"] == "run_command")
    );
    assert!(
        json["recent_history"]["recent_failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "run_command:execution")
    );
}

#[tokio::test]
async fn classify_preview_uses_learned_route_when_available() {
    let state = test_state().await;
    state
        .learned_router()
        .ingest(&learned_record(
            TaskIntent::Coding,
            "primary",
            vec!["rust".into()],
        ))
        .unwrap();

    let app = build_app(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/orchestration/classify")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "message": "write a function to sort a list in rust" }).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["routing"]["target_agent"], "primary");
    assert!(json["routing"]["reason"].as_str().is_some());
}

#[tokio::test]
async fn learned_routes_endpoint_supports_filtering_and_clear() {
    let state = test_state().await;
    state
        .learned_router()
        .ingest(&learned_record(
            TaskIntent::Coding,
            "primary",
            vec!["rust".into()],
        ))
        .unwrap();
    state
        .learned_router()
        .ingest(&learned_record(TaskIntent::Analysis, "primary", vec![]))
        .unwrap();

    let app = build_app(state.clone());
    let req = Request::builder()
        .uri("/api/v1/orchestration/learned?intent=Coding")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["count"], 1);
    assert_eq!(json["rules"][0]["rule"]["intent"], "Coding");
    assert!(json["rules"][0]["adaptive_decay_multiplier"].is_number());
    assert!(json["rules"][0]["accept_rate"].is_number());
    assert!(
        json["history"]["intents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["label"] == "Coding")
    );

    let delete_req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/orchestration/learned?intent=Coding")
        .body(Body::empty())
        .unwrap();
    let delete_resp = app.clone().oneshot(delete_req).await.unwrap();
    assert_eq!(delete_resp.status(), StatusCode::OK);

    let verify_req = Request::builder()
        .uri("/api/v1/orchestration/learned?include_penalized=true")
        .body(Body::empty())
        .unwrap();
    let verify_resp = app.oneshot(verify_req).await.unwrap();
    assert_eq!(verify_resp.status(), StatusCode::OK);

    let verify_body = axum::body::to_bytes(verify_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let verify_json: Value = serde_json::from_slice(&verify_body).unwrap();
    assert_eq!(verify_json["count"], 1);
    assert_eq!(verify_json["rules"][0]["rule"]["intent"], "Analysis");
    assert!(
        verify_json["history"]["intents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["label"] == "Analysis")
    );
}

#[tokio::test]
async fn learned_routes_endpoint_reports_stale_rules() {
    let state = test_state().await;
    let mut stale = learned_record(TaskIntent::Coding, "primary", vec!["rust".into()]);
    stale.timestamp = chrono::Utc::now() - chrono::Duration::days(120);
    state.learned_router().ingest(&stale).unwrap();

    let app = build_app(state);
    let req = Request::builder()
        .uri("/api/v1/orchestration/learned?include_penalized=true&stale_only=true")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["count"], 1);
    assert_eq!(json["rules"][0]["stale"], true);
    assert!(json["rules"][0]["age_days"].as_u64().unwrap() >= 120);
}

#[tokio::test]
async fn status_reports_sandbox_policy_details() {
    let mut config = NgenOrcaConfig::default();
    config.sandbox.policy.allow_network = true;
    config.sandbox.policy.allow_workspace_write = false;
    config.sandbox.policy.additional_read_paths = vec!["/tmp/ngenorca-read".into()];
    config.sandbox.policy.additional_write_paths = vec!["/tmp/ngenorca-write".into()];
    config.sandbox.policy.memory_limit_mb = 256;
    config.sandbox.policy.cpu_limit_seconds = 12;
    config.sandbox.policy.wall_time_limit_seconds = 20;

    let app = build_app(state_with_config(config).await);
    let req = Request::builder()
        .uri("/api/v1/status")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["sandbox"]["policy"]["allow_network"], true);
    assert_eq!(json["sandbox"]["policy"]["allow_workspace_write"], false);
    assert_eq!(json["sandbox"]["policy"]["memory_limit_mb"], 256);
    assert_eq!(json["sandbox"]["policy"]["cpu_limit_seconds"], 12);
    assert_eq!(json["sandbox"]["policy"]["wall_time_limit_seconds"], 20);
    assert_eq!(
        json["sandbox"]["policy"]["additional_read_paths"][0],
        "/tmp/ngenorca-read"
    );
    assert_eq!(
        json["sandbox"]["policy"]["additional_write_paths"][0],
        "/tmp/ngenorca-write"
    );
    assert!(json["sandbox"]["requested_backend"].is_string());
    assert!(json["sandbox"]["audit"]["backend"].is_string());
    assert!(json["events"]["retention_days"].is_u64());
}

#[tokio::test]
async fn event_history_endpoint_supports_filtered_failures() {
    let state = test_state().await;
    seed_operator_history_events(&state).await;

    let app = build_app(state);
    let req = Request::builder()
        .uri("/api/v1/events/history?kind=tool_execution&tool_name=run_command&failure_only=true&since_hours=48")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["matched_events"], 1);
    assert_eq!(json["events"][0]["event_kind"], "tool_execution");
    assert_eq!(json["events"][0]["summary"]["tool_name"], "run_command");
    assert_eq!(json["history"]["tool_event_count"], 1);
    assert!(
        json["history"]["recent_failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "run_command:execution")
    );
}

#[tokio::test]
async fn correction_timeline_endpoint_groups_longer_horizon_activity() {
    let state = test_state().await;
    seed_operator_history_events(&state).await;

    let app = build_app(state);
    let req = Request::builder()
        .uri("/api/v1/events/timeline/corrections?channel=web&since_hours=72&bucket_hours=24")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert!(json["bucket_count"].as_u64().unwrap() >= 1);
    assert!(
        json["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["correction_attempts"].as_u64().unwrap() > 0)
    );
    assert!(
        json["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bucket| bucket["tool_failures"].as_u64().unwrap() > 0)
    );
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
    let id = resp
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
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
    let echo = resp
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
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
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    let state = AppState::new(AppStateParams {
        config,
        config_file_path: temp_dir.join("config.toml"),
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
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    let state = AppState::new(AppStateParams {
        config,
        config_file_path: temp_dir.join("config.toml"),
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

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
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
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    let state = AppState::new(AppStateParams {
        config,
        config_file_path: temp_dir.join("config.toml"),
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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
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

// ─── SEC-03: TrustedProxy source IP check ────────────────────────────

/// Helper to build state with a custom config.
async fn state_with_config(config: NgenOrcaConfig) -> AppState {
    let event_bus = EventBus::new(":memory:").await.unwrap();
    let identity = IdentityManager::new(":memory:").unwrap();
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_dir = std::env::temp_dir().join(format!(
        "ngenorca_integ_custom_{}_{unique}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let memory = MemoryManager::new(temp_dir.to_str().unwrap()).unwrap();
    let providers = ProviderRegistry::from_config(&config);
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let plugins = PluginRegistry::new(tx, std::path::PathBuf::from("/tmp/test_plugins"));
    let learned_router = LearnedRouter::new(":memory:").unwrap();
    let metrics = Metrics::new();
    AppState::new(AppStateParams {
        config,
        config_file_path: temp_dir.join("config.toml"),
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
async fn trusted_proxy_rejects_without_connect_info() {
    // SEC-03: When TrustedProxy mode is active with a source allowlist,
    // requests without ConnectInfo (i.e. unknown source) must be rejected.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::TrustedProxy;
    // trusted_proxy_sources defaults to ["127.0.0.1", "::1"]

    let app = build_app(state_with_config(config).await);

    // Request with Remote-User header but no ConnectInfo → 403.
    let req = Request::builder()
        .uri("/api/v1/status")
        .header("Remote-User", "spoofed-user")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn trusted_proxy_rejects_untrusted_source_ip() {
    // SEC-03: Requests from non-allowlisted IPs must be rejected.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::TrustedProxy;
    config.gateway.trusted_proxy_sources = vec!["10.0.0.1".into()];

    let app = build_app(state_with_config(config).await);

    // Inject ConnectInfo with an untrusted IP.
    let mut req = Request::builder()
        .uri("/api/v1/status")
        .header("Remote-User", "spoofed-user")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "192.168.1.100:12345"
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn trusted_proxy_accepts_trusted_source_ip() {
    // SEC-03: Requests from an allowlisted IP with valid headers should pass.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::TrustedProxy;
    config.gateway.trusted_proxy_sources = vec!["10.0.0.1".into()];

    let app = build_app(state_with_config(config).await);

    let mut req = Request::builder()
        .uri("/api/v1/status")
        .header("Remote-User", "legitimate-user")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "10.0.0.1:54321".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn trusted_proxy_accepts_ip_within_cidr_range() {
    // SEC-03: CIDR ranges like "192.168.0.0/16" must match IPs in that subnet.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::TrustedProxy;
    config.gateway.trusted_proxy_sources = vec!["192.168.0.0/16".into()];

    let app = build_app(state_with_config(config).await);

    let mut req = Request::builder()
        .uri("/api/v1/status")
        .header("Remote-User", "cidr-user")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "192.168.1.100:9999"
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn trusted_proxy_rejects_ip_outside_cidr_range() {
    // SEC-03: IPs outside the CIDR range must be rejected.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::TrustedProxy;
    config.gateway.trusted_proxy_sources = vec!["10.0.0.0/8".into()];

    let app = build_app(state_with_config(config).await);

    let mut req = Request::builder()
        .uri("/api/v1/status")
        .header("Remote-User", "outside-cidr")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        "192.168.1.1:9999".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ─── SEC-05: Webhook fail-closed checks ──────────────────────────────

#[tokio::test]
async fn telegram_webhook_rejects_missing_secret_header() {
    // SEC-05: When bot_token is configured, the secret token header is required.
    let mut config = NgenOrcaConfig::default();
    config.channels.telegram = Some(ngenorca_config::TelegramChannelConfig {
        enabled: true,
        bot_token: Some("test-bot-token-12345".into()),
        webhook_url: None,
        polling: false,
        allowed_users: vec![],
    });

    let app = build_app(state_with_config(config).await);

    // POST without x-telegram-bot-api-secret-token header → 401.
    let req = Request::builder()
        .uri("/webhooks/telegram")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(b"{}".as_slice()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn telegram_webhook_rejects_invalid_secret() {
    // SEC-05: An incorrect secret token must be rejected.
    let mut config = NgenOrcaConfig::default();
    config.channels.telegram = Some(ngenorca_config::TelegramChannelConfig {
        enabled: true,
        bot_token: Some("test-bot-token-12345".into()),
        webhook_url: None,
        polling: false,
        allowed_users: vec![],
    });

    let app = build_app(state_with_config(config).await);

    let req = Request::builder()
        .uri("/webhooks/telegram")
        .method("POST")
        .header("content-type", "application/json")
        .header("x-telegram-bot-api-secret-token", "wrong-token")
        .body(Body::from(b"{}".as_slice()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn teams_webhook_rejects_missing_auth_header() {
    // SEC-05: Teams webhook rejects when Authorization header is absent.
    let app = build_app(test_state().await);

    let req = Request::builder()
        .uri("/webhooks/teams")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(b"{}".as_slice()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn teams_webhook_rejects_invalid_jwt() {
    // SEC-05: Teams webhook rejects a malformed JWT.
    let app = build_app(test_state().await);

    let req = Request::builder()
        .uri("/webhooks/teams")
        .method("POST")
        .header("content-type", "application/json")
        .header("Authorization", "Bearer not-a-valid-jwt")
        .body(Body::from(b"{}".as_slice()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── PRIV-03: Cross-user data deletion (IDOR) ───────────────────────

#[tokio::test]
async fn dsar_delete_rejects_cross_user_request() {
    // PRIV-03: User "alice" must not be able to delete data for "bob".
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::Password;
    config.gateway.auth_password = Some("test-password".into());

    let app = build_app(state_with_config(config).await);

    // Authenticate as "alice" and attempt to delete "bob"'s data.
    let credentials = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        b"alice:test-password",
    );
    let req = Request::builder()
        .uri("/api/v1/memory/user/bob")
        .method("DELETE")
        .header("Authorization", format!("Basic {credentials}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "You may only delete your own data");
}

// ─── SEC-06: Webhook auth exemption under TrustedProxy ──────────────

#[tokio::test]
async fn webhook_is_reachable_in_trusted_proxy_mode_without_proxy_headers() {
    // SEC-06: Webhook routes must bypass TrustedProxy auth middleware so that
    // third-party providers (Telegram, Teams, etc.) can POST callbacks even
    // without reverse-proxy identity headers.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::TrustedProxy;
    // NB: trusted_proxy_sources defaults to loopback only. The webhook POST
    // will have no ConnectInfo → would get 403 if auth middleware were applied.
    // Channel-specific verification still rejects (401), but that proves the
    // request passed *auth middleware* and reached the webhook handler.

    let app = build_app(state_with_config(config).await);

    let req = Request::builder()
        .uri("/webhooks/teams")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(b"{}".as_slice()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // 401 (channel verification fails) — NOT 403 (auth middleware block).
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── SEC-05: Teams audience enforcement ─────────────────────────────

#[tokio::test]
async fn teams_webhook_rejects_when_app_id_missing_from_config() {
    // SEC-05: When teams.app_id is not configured, the JWT verifier must
    // reject (fail-closed on missing audience) rather than skip aud validation.
    let mut config = NgenOrcaConfig::default();
    config.channels.teams = Some(ngenorca_config::TeamsChannelConfig {
        enabled: true,
        app_id: None, // <-- no audience configured
        app_password: Some("secret".into()),
        tenant_id: "common".into(),
        webhook_url: None,
    });

    let app = build_app(state_with_config(config).await);

    // Send a structurally valid RS256 JWT — must still be rejected.
    let header = base64_url_encode(r#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#);
    let payload = base64_url_encode(r#"{"iss":"https://api.botframework.com","exp":9999999999}"#);
    let token = format!("Bearer {header}.{payload}.fake-sig");

    let req = Request::builder()
        .uri("/webhooks/teams")
        .method("POST")
        .header("content-type", "application/json")
        .header("Authorization", &token)
        .body(Body::from(b"{}".as_slice()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// base64url encode helper (no padding).
fn base64_url_encode(input: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input.as_bytes())
}

// ─── SEC-02: WebSocket event authorization isolation ─────────────────

/// Spin up a real TCP server with the given config and return the bound address.
/// The server runs in a background task and is cancelled when the returned
/// `tokio::task::JoinHandle` is aborted.
async fn start_test_server(
    config: NgenOrcaConfig,
) -> (std::net::SocketAddr, AppState, tokio::task::JoinHandle<()>) {
    let state = state_with_config(config).await;
    let app = build_app(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give the server a moment to accept connections.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (addr, state, handle)
}

/// Helper: connect a WS client with Basic auth credentials.
async fn ws_connect_with_auth(
    addr: std::net::SocketAddr,
    user: &str,
    password: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use base64::Engine;
    let creds = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    let url = format!("ws://{addr}/ws");
    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Basic {creds}"))
        .header("Host", addr.to_string())
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();

    let (ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("WS handshake failed");
    ws
}

/// Helper: connect an anonymous WS client (no auth headers, `auth_mode=None`).
async fn ws_connect_anonymous(
    addr: std::net::SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{addr}/ws");
    let (ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WS handshake failed");
    ws
}

/// Helper: drain the "connected" welcome message from a freshly opened WS.
async fn drain_welcome(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    use futures::StreamExt;
    // The first message is always the welcome JSON.
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
        .await
        .expect("timeout waiting for welcome")
        .expect("stream ended")
        .expect("WS error");
    let text = msg.into_text().expect("expected text message");
    assert!(
        text.contains("connected"),
        "expected welcome message, got: {text}"
    );
}

/// Helper: try to receive a WS text message within a timeout.
/// Returns `Some(text)` or `None` if the timeout elapses.
async fn try_recv_ws(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    timeout_ms: u64,
) -> Option<String> {
    use futures::StreamExt;
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), ws.next()).await {
        Ok(Some(Ok(msg))) => msg.into_text().ok().map(|s| s.to_string()),
        _ => None,
    }
}

#[tokio::test]
async fn ws_user_scoped_event_not_visible_to_other_user() {
    // SEC-02: Alice must NOT see events scoped to Bob.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::Password;
    config.gateway.auth_password = Some("testpw".into());

    let (addr, state, server_handle) = start_test_server(config).await;

    // Connect Alice and Bob.
    let mut alice_ws = ws_connect_with_auth(addr, "alice", "testpw").await;
    let mut bob_ws = ws_connect_with_auth(addr, "bob", "testpw").await;
    drain_welcome(&mut alice_ws).await;
    drain_welcome(&mut bob_ws).await;

    // Publish an event scoped to Alice.
    let alice_event = ngenorca_core::event::Event {
        id: ngenorca_core::types::EventId::new(),
        timestamp: chrono::Utc::now(),
        session_id: None,
        user_id: Some(ngenorca_core::types::UserId("alice".into())),
        payload: ngenorca_core::event::EventPayload::PluginLoaded {
            plugin_id: ngenorca_core::PluginId("test-alice".into()),
            version: "test-alice-v1".into(),
        },
    };
    state.event_bus().publish(alice_event).await.unwrap();

    // Alice should receive the event.
    let alice_msg = try_recv_ws(&mut alice_ws, 2000).await;
    assert!(alice_msg.is_some(), "Alice should receive her own event");
    let alice_text = alice_msg.unwrap();
    assert!(
        alice_text.contains("test-alice-v1"),
        "Alice got wrong event: {alice_text}"
    );

    // Bob should NOT receive Alice's event (give a generous timeout).
    let bob_msg = try_recv_ws(&mut bob_ws, 500).await;
    assert!(
        bob_msg.is_none(),
        "Bob must NOT see Alice's event, but got: {:?}",
        bob_msg
    );

    server_handle.abort();
}

#[tokio::test]
async fn ws_system_event_visible_to_all_users() {
    // SEC-02: System-level events (user_id=None) should be broadcast to all.
    let mut config = NgenOrcaConfig::default();
    config.gateway.auth_mode = ngenorca_config::AuthMode::Password;
    config.gateway.auth_password = Some("testpw".into());

    let (addr, state, server_handle) = start_test_server(config).await;

    let mut alice_ws = ws_connect_with_auth(addr, "alice", "testpw").await;
    let mut bob_ws = ws_connect_with_auth(addr, "bob", "testpw").await;
    drain_welcome(&mut alice_ws).await;
    drain_welcome(&mut bob_ws).await;

    // Publish a system-level event (no user_id).
    let sys_event = ngenorca_core::event::Event {
        id: ngenorca_core::types::EventId::new(),
        timestamp: chrono::Utc::now(),
        session_id: None,
        user_id: None,
        payload: ngenorca_core::event::EventPayload::PluginLoaded {
            plugin_id: ngenorca_core::PluginId("sys-plug".into()),
            version: "system-broadcast-v1".into(),
        },
    };
    state.event_bus().publish(sys_event).await.unwrap();

    // Both Alice and Bob should receive the system event.
    let alice_msg = try_recv_ws(&mut alice_ws, 2000).await;
    assert!(alice_msg.is_some(), "Alice should receive system event");
    assert!(alice_msg.unwrap().contains("system-broadcast-v1"));

    let bob_msg = try_recv_ws(&mut bob_ws, 2000).await;
    assert!(bob_msg.is_some(), "Bob should receive system event");
    assert!(bob_msg.unwrap().contains("system-broadcast-v1"));

    server_handle.abort();
}

#[tokio::test]
async fn ws_anonymous_does_not_receive_user_scoped_events() {
    // SEC-02: Anonymous connections must NOT receive user-scoped events.
    let mut config = NgenOrcaConfig::default();
    // auth_mode=None allows anonymous WS connections.
    config.gateway.auth_mode = ngenorca_config::AuthMode::None;

    let (addr, state, server_handle) = start_test_server(config).await;

    let mut anon_ws = ws_connect_anonymous(addr).await;
    drain_welcome(&mut anon_ws).await;

    // Publish a user-scoped event.
    let user_event = ngenorca_core::event::Event {
        id: ngenorca_core::types::EventId::new(),
        timestamp: chrono::Utc::now(),
        session_id: None,
        user_id: Some(ngenorca_core::types::UserId("alice".into())),
        payload: ngenorca_core::event::EventPayload::PluginLoaded {
            plugin_id: ngenorca_core::PluginId("user-only".into()),
            version: "user-only-v1".into(),
        },
    };
    state.event_bus().publish(user_event).await.unwrap();

    // Anonymous connection should NOT receive user-scoped event.
    let anon_msg = try_recv_ws(&mut anon_ws, 500).await;
    assert!(
        anon_msg.is_none(),
        "Anonymous must NOT see user-scoped event, got: {:?}",
        anon_msg
    );

    // But system events SHOULD reach anonymous connections.
    let sys_event = ngenorca_core::event::Event {
        id: ngenorca_core::types::EventId::new(),
        timestamp: chrono::Utc::now(),
        session_id: None,
        user_id: None,
        payload: ngenorca_core::event::EventPayload::PluginLoaded {
            plugin_id: ngenorca_core::PluginId("sys-anon".into()),
            version: "sys-anon-v1".into(),
        },
    };
    state.event_bus().publish(sys_event).await.unwrap();

    let sys_msg = try_recv_ws(&mut anon_ws, 2000).await;
    assert!(sys_msg.is_some(), "Anonymous should receive system events");
    assert!(sys_msg.unwrap().contains("sys-anon-v1"));

    server_handle.abort();
}
