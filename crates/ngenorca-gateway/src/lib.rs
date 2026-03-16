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
pub mod config_ui;
pub mod metrics;
pub mod orchestration;
pub mod plugins;
pub mod providers;
pub mod rate_limit;
pub mod request_id;
pub mod routes;
pub(crate) mod runtime_identity;
pub mod server;
pub mod sessions;
pub mod skills;
pub mod state;
pub mod tools;

use ngenorca_bus::EventBus;
use ngenorca_config::NgenOrcaConfig;
use ngenorca_core::Result;
use ngenorca_identity::IdentityManager;
use ngenorca_identity::resolver::IdentityAction;
use ngenorca_memory::MemoryManager;
use plugins::PluginRegistry;
use providers::ProviderRegistry;
use sessions::SessionManager;
use tracing::info;

/// Start the NgenOrca gateway.
pub async fn start(config: NgenOrcaConfig, config_file_path: std::path::PathBuf) -> Result<()> {
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
    let sessions = SessionManager::new(config.agent.model.clone(), config.agent.thinking_level);

    // Initialize plugin registry.
    // Create an mpsc channel that bridges plugin events → EventBus.
    let (plugin_tx, mut plugin_rx) = tokio::sync::mpsc::unbounded_channel();
    let plugin_dir = config.data_dir.join("plugins");
    std::fs::create_dir_all(&plugin_dir).ok();
    let plugin_registry =
        PluginRegistry::new_with_sandbox(plugin_tx, plugin_dir, config.sandbox.enabled);
    info!("Plugin registry initialized");

    // Register built-in agent tools.
    tools::register_builtin_tools(&plugin_registry, &config).await;
    info!(
        tool_count = plugin_registry.tool_count().await,
        "Built-in tools registered"
    );

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
    std::fs::create_dir_all(&config.agent.workspace).ok();
    let learned_db_path = config.agent.workspace.join("learned_routes.db");
    let learned_router =
        orchestration::LearnedRouter::new(learned_db_path.to_str().unwrap_or("learned_routes.db"))?;

    // Initialize metrics registry.
    let metrics_registry = metrics::Metrics::new();

    // Build shared application state.
    let app_state = state::AppState::new(state::AppStateParams {
        config: config.clone(),
        config_file_path,
        event_bus,
        identity,
        memory,
        providers,
        sessions,
        plugins: plugin_registry,
        learned_router,
        metrics: metrics_registry,
    });

    // Register channel adapters from config.
    channels::register_adapters(&config, app_state.plugins()).await?;
    info!(
        adapters = ?config.enabled_channels(),
        "Channel adapters registered"
    );

    // Start listener loops for all registered channel adapters.
    // Each adapter's start_listening() is spawned as an independent task.
    app_state.plugins().start_all_adapters().await;

    // ── Inbound message worker ───────────────────────────────────────
    //
    // Subscribes to the EventBus and processes inbound messages from
    // channel adapters through the same orchestration pipeline used by
    // the HTTP /api/v1/chat endpoint.  Replies are routed back to the
    // originating adapter via `PluginRegistry::route_to_adapter()`.
    {
        let state = app_state.clone();
        let mut rx = state.event_bus().subscribe();
        tokio::spawn(async move {
            loop {
                let event = match rx.recv().await {
                    Ok(e) => e,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "Inbound worker lagged — some events dropped");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Event bus closed — inbound worker shutting down");
                        break;
                    }
                };

                // Only process inbound messages from channel adapters.
                let msg = match &event.payload {
                    ngenorca_core::event::EventPayload::Message(m)
                        if m.direction == ngenorca_core::message::Direction::Inbound =>
                    {
                        m.clone()
                    }
                    _ => continue,
                };

                // Extract text content (skip non-text messages for now).
                let text = match &msg.content {
                    ngenorca_core::message::Content::Text(t) => t.clone(),
                    ngenorca_core::message::Content::Image {
                        caption: Some(c), ..
                    } => c.clone(),
                    ngenorca_core::message::Content::Audio {
                        transcript: Some(t),
                        ..
                    } => t.clone(),
                    _ => {
                        tracing::debug!(channel = %msg.channel_kind, "Skipping non-text inbound message");
                        continue;
                    }
                };

                let channel_kind_str = msg.channel_kind.to_string();
                let resolved_identity =
                    runtime_identity::resolve_message_identity(state.identity(), &msg);
                let identity_diagnostics =
                    runtime_identity::describe_message_identity(&msg, &resolved_identity);
                if matches!(
                    resolved_identity.action,
                    IdentityAction::Challenge | IdentityAction::Block
                ) {
                    tracing::warn!(
                        channel = %channel_kind_str,
                        action = ?resolved_identity.action,
                        user = ?resolved_identity.user_id,
                        reason = %identity_diagnostics.reason,
                        suggested_actions = ?identity_diagnostics.suggested_actions,
                        "Skipping inbound message due to failed device verification"
                    );
                    continue;
                }
                if matches!(
                    resolved_identity.action,
                    IdentityAction::RequirePairing | IdentityAction::ProceedReduced
                ) {
                    tracing::info!(
                        channel = %channel_kind_str,
                        action = ?resolved_identity.action,
                        user = ?resolved_identity.user_id,
                        reason = %identity_diagnostics.reason,
                        suggested_actions = ?identity_diagnostics.suggested_actions,
                        "Inbound message is proceeding with identity follow-up guidance"
                    );
                }
                let user_id = resolved_identity.user_id.clone();

                tracing::info!(
                    channel = %channel_kind_str,
                    user = ?user_id,
                    trust = ?resolved_identity.trust,
                    len = text.len(),
                    "Processing inbound channel message"
                );

                // Get or create session for this user + channel.
                let session_id = if let (Some(alias_user), Some(canonical_user)) =
                    (msg.user_id.as_ref(), user_id.as_ref())
                {
                    if alias_user != canonical_user {
                        match state.sessions().promote_alias_to_user(
                            alias_user,
                            &channel_kind_str,
                            canonical_user,
                        ) {
                            Ok(Some(session_id)) => session_id,
                            Ok(None) => match state
                                .sessions()
                                .get_or_create(user_id.as_ref(), &channel_kind_str)
                            {
                                Ok(session_id) => session_id,
                                Err(e) => {
                                    tracing::error!(error = %e, "Failed to create session for inbound message");
                                    continue;
                                }
                            },
                            Err(e) => {
                                tracing::warn!(error = %e, alias = %alias_user, canonical = %canonical_user, "Failed to promote alias session; falling back to canonical lookup");
                                match state
                                    .sessions()
                                    .get_or_create(user_id.as_ref(), &channel_kind_str)
                                {
                                    Ok(session_id) => session_id,
                                    Err(e) => {
                                        tracing::error!(error = %e, "Failed to create session for inbound message");
                                        continue;
                                    }
                                }
                            }
                        }
                    } else {
                        match state
                            .sessions()
                            .get_or_create(user_id.as_ref(), &channel_kind_str)
                        {
                            Ok(session_id) => session_id,
                            Err(e) => {
                                tracing::error!(error = %e, "Failed to create session for inbound message");
                                continue;
                            }
                        }
                    }
                } else {
                    match state
                        .sessions()
                        .get_or_create(user_id.as_ref(), &channel_kind_str)
                    {
                        Ok(session_id) => session_id,
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to create session for inbound message");
                            continue;
                        }
                    }
                };

                let orch = orchestration::HybridOrchestrator::new(std::sync::Arc::new(
                    state.config().clone(),
                ));
                let classification = match orch.classify(&text, Some(state.providers())).await {
                    Ok(classification) => classification,
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to classify inbound message");
                        continue;
                    }
                };

                // Build memory context.
                let memory_ctx = if state.config().memory.enabled {
                    if let Some(ref uid) = user_id {
                        let budget = state.config().memory.semantic_token_budget;
                        state
                            .memory()
                            .build_context_for_task(
                                uid,
                                &session_id,
                                &text,
                                &classification,
                                budget,
                            )
                            .ok()
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Store user message in working memory.
                state.memory().working.push(
                    &session_id,
                    ngenorca_memory::working::WorkingMessage {
                        role: "user".into(),
                        content: text.clone(),
                        timestamp: chrono::Utc::now(),
                        estimated_tokens: text.len() / 4,
                    },
                );

                // Build conversation from prior working memory.
                let conversation: Vec<ngenorca_plugin_sdk::ChatMessage> = memory_ctx
                    .as_ref()
                    .map(|ctx| {
                        ctx.working_messages
                            .iter()
                            .map(|wm| ngenorca_plugin_sdk::ChatMessage {
                                role: wm.role.clone(),
                                content: wm.content.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                // Orchestrate.
                match orch
                    .process_with_classification(
                        &text,
                        &classification,
                        &conversation,
                        state.providers(),
                        Some(state.plugins()),
                        memory_ctx.as_ref(),
                        orchestration::InvocationContext {
                            learned_router: Some(state.learned_router()),
                            session_id: Some(&session_id),
                            user_id: user_id.as_ref(),
                            channel: Some(&channel_kind_str),
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

                        // Ingest learned routing rule.
                        if let Err(e) = state.learned_router().ingest(&record) {
                            tracing::warn!(error = %e, "Failed to ingest learned routing rule");
                        }

                        // Store assistant response in working memory.
                        state.memory().working.push(
                            &session_id,
                            ngenorca_memory::working::WorkingMessage {
                                role: "assistant".into(),
                                content: response.content.clone(),
                                timestamp: chrono::Utc::now(),
                                estimated_tokens: response.content.len() / 4,
                            },
                        );

                        // Update session.
                        let _ = state
                            .sessions()
                            .record_message(&session_id, response.total_usage.total_tokens);

                        // Store in episodic memory.
                        if let Some(ref uid) = user_id {
                            let entry = ngenorca_memory::episodic::EpisodicEntry {
                                id: 0,
                                user_id: uid.0.clone(),
                                content: format!("User: {}\nAssistant: {}", text, response.content),
                                summary: None,
                                channel: channel_kind_str.clone(),
                                timestamp: chrono::Utc::now(),
                                embedding: None,
                                relevance_score: 0.0,
                            };
                            if let Err(e) = state.memory().episodic.store(&entry) {
                                tracing::warn!(error = %e, "Failed to store episodic memory");
                            }
                        }

                        // Build the reply Message and route it to the adapter.
                        let reply = ngenorca_core::message::Message {
                            id: ngenorca_core::types::EventId::new(),
                            timestamp: chrono::Utc::now(),
                            user_id: user_id.clone(),
                            trust: resolved_identity.trust,
                            session_id: session_id.clone(),
                            channel: msg.channel.clone(),
                            channel_kind: msg.channel_kind.clone(),
                            direction: ngenorca_core::message::Direction::Outbound,
                            content: ngenorca_core::message::Content::Text(
                                response.content.clone(),
                            ),
                            metadata: serde_json::Value::Null,
                        };

                        if let Err(e) = state
                            .plugins()
                            .route_to_adapter(&channel_kind_str, &reply)
                            .await
                        {
                            tracing::error!(
                                channel = %channel_kind_str,
                                error = %e,
                                "Failed to route reply to adapter"
                            );
                        }

                        // Publish orchestration event for analytics.
                        let orch_event = ngenorca_core::event::Event {
                            id: ngenorca_core::types::EventId::new(),
                            timestamp: chrono::Utc::now(),
                            session_id: Some(session_id),
                            user_id,
                            payload: ngenorca_core::event::EventPayload::OrchestrationCompleted(
                                record,
                            ),
                        };
                        if let Err(e) = state.event_bus().publish(orch_event).await {
                            tracing::warn!(error = %e, "Failed to publish orchestration record");
                        }
                    }
                    Err(e) => {
                        state.metrics().inc_channel_errors();
                        tracing::error!(
                            channel = %channel_kind_str,
                            error = %e,
                            "Inbound message orchestration failed"
                        );
                    }
                }
            }
        });
        info!("Inbound message worker started");
    }

    // Spawn background memory consolidation task.
    {
        let state = app_state.clone();
        let consolidation_interval =
            std::time::Duration::from_secs(config.memory.consolidation_interval_secs);
        let max_episodes = config.memory.episodic_max_entries;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(consolidation_interval);
            interval.tick().await; // Skip the immediate first tick
            loop {
                interval.tick().await;
                tracing::debug!("Running memory consolidation cycle");

                // Get distinct user IDs from episodic memory
                let users = state.memory().episodic.distinct_users().unwrap_or_default();

                for user_id in users {
                    let uid = ngenorca_core::types::UserId(user_id);
                    match state.memory().consolidate_for_user(
                        &uid,
                        chrono::Duration::hours(24),
                        max_episodes,
                    ) {
                        Ok(_) => {
                            state.metrics().inc_consolidations();
                        }
                        Err(e) => {
                            tracing::warn!(
                                user = %uid,
                                error = %e,
                                "Memory consolidation failed for user"
                            );
                        }
                    }
                }
            }
        });
        info!(
            interval_secs = consolidation_interval.as_secs(),
            "Memory consolidation task started"
        );
    }

    // Spawn background session pruning task.
    {
        let state = app_state.clone();
        let prune_interval = config.gateway.session_prune_interval_secs;
        let session_ttl = config.gateway.session_ttl_secs;
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(prune_interval));
            interval.tick().await; // Skip immediate first tick
            loop {
                interval.tick().await;
                let pruned = state
                    .sessions()
                    .prune_expired(std::time::Duration::from_secs(session_ttl));
                if pruned > 0 {
                    tracing::debug!(pruned, "Session pruning cycle complete");
                }
            }
        });
        info!(
            interval_secs = config.gateway.session_prune_interval_secs,
            ttl_secs = config.gateway.session_ttl_secs,
            "Session pruning task started",
        );
    }

    // Spawn background event log pruning task.
    {
        let state = app_state.clone();
        let prune_interval = config.gateway.event_log_prune_interval_secs;
        let retention_days = config.gateway.event_log_retention_days;
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(prune_interval));
            interval.tick().await; // Skip immediate first tick
            loop {
                interval.tick().await;
                let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
                match state.event_bus().event_log().prune_before(cutoff) {
                    Ok(deleted) => {
                        if deleted > 0 {
                            tracing::info!(deleted, "Event log pruning complete");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Event log pruning failed");
                    }
                }
            }
        });
        info!(
            interval_secs = config.gateway.event_log_prune_interval_secs,
            retention_days = config.gateway.event_log_retention_days,
            "Event log pruning task started",
        );
    }

    // Start the HTTP/WebSocket server (blocks until shutdown signal).
    server::run(app_state.clone(), &config.gateway.bind, config.gateway.port).await?;

    // Graceful cleanup.
    info!("Shutting down plugins…");
    app_state.plugins().shutdown_all().await;
    info!("All plugins stopped. Goodbye.");

    Ok(())
}
