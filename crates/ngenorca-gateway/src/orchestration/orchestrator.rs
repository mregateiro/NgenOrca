//! Hybrid agent orchestrator implementation.
//!
//! The orchestrator manages the full pipeline:
//! 1. Classify the task (rules → SLM → LLM)
//! 2. Route to the best sub-agent
//! 3. Generate a dynamic system prompt tailored to the task
//! 4. Delegate to the sub-agent
//! 5. Quality-check the response
//! 6. Escalate or augment if needed
//! 7. Record the result for learning

use ngenorca_config::{NgenOrcaConfig, RoutingStrategy, SubAgentConfig};
use ngenorca_core::orchestration::{
    ClassificationMethod, OrchestrationRecord, QualityMethod,
    QualityVerdict, RoutingDecision, SubAgentId, TaskClassification,
    TaskComplexity, TaskIntent,
};
use ngenorca_core::Result;
use ngenorca_plugin_sdk::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, OrchestratedResponse,
    ToolDefinition, Usage,
};
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::classifier::RuleBasedClassifier;
use super::quality::HeuristicQualityGate;
use crate::plugins::PluginRegistry;
use crate::providers::ProviderRegistry;

/// The hybrid orchestrator — combines rule-based classification with
/// quality gating and learned routing rules.
pub struct HybridOrchestrator {
    config: Arc<NgenOrcaConfig>,
    classifier: RuleBasedClassifier,
    quality_gate: HeuristicQualityGate,
}

impl HybridOrchestrator {
    /// Create a new orchestrator from the application config.
    pub fn new(config: Arc<NgenOrcaConfig>) -> Self {
        let quality_gate = HeuristicQualityGate::from_config(&config.agent.quality_gate);

        Self {
            config,
            classifier: RuleBasedClassifier::new(),
            quality_gate,
        }
    }

    /// Classify a message using the cascading strategy.
    ///
    /// Level 1: Rule-based (zero cost)
    /// Level 2: SLM classifier via provider (low cost — small model)
    /// Level 3: Return best-effort classification for the orchestrator to handle
    pub async fn classify(
        &self,
        message: &str,
        registry: Option<&ProviderRegistry>,
    ) -> Result<TaskClassification> {
        use ngenorca_plugin_sdk::TaskClassifier;

        // Level 1: Rule-based (zero cost)
        let classification = self.classifier.classify(message, None).await?;

        match &self.config.agent.routing {
            RoutingStrategy::RuleBased | RoutingStrategy::Single => {
                // For rule-based or single mode, don't escalate classification
                return Ok(classification);
            }
            _ => {}
        }

        // If rule-based is confident enough, use it
        let threshold = self
            .config
            .agent
            .classifier
            .as_ref()
            .map(|c| c.confidence_threshold)
            .unwrap_or(0.8);

        if classification.confidence >= threshold
            && classification.intent != TaskIntent::Unknown
        {
            debug!(
                intent = ?classification.intent,
                confidence = classification.confidence,
                "Rule-based classification accepted (confidence >= {threshold})"
            );
            return Ok(classification);
        }

        // Level 2: SLM classifier (if configured and provider available)
        if let (Some(classifier_cfg), Some(registry)) =
            (&self.config.agent.classifier, registry)
        {
            debug!(
                rule_confidence = classification.confidence,
                "Rule-based confidence too low, invoking SLM classifier"
            );

            // Use the classifier model (or fall back to primary model)
            let classifier_model = if classifier_cfg.model.is_empty() {
                self.config.agent.model.clone()
            } else {
                classifier_cfg.model.clone()
            };

            let prompt = format!(
                "Classify this user message into exactly one intent category. \
                 Respond with ONLY the category name, nothing else.\n\n\
                 Categories: summarization, translation, coding, analysis, creative, \
                 extraction, reasoning, planning, question_answering, unknown\n\n\
                 Also rate the complexity: trivial, simple, moderate, complex, expert\n\n\
                 Format: <intent>|<complexity>\n\n\
                 User message: {message}"
            );

            let request = ChatCompletionRequest {
                model: classifier_model,
                messages: vec![
                    ChatMessage {
                        role: "system".into(),
                        content: "You are a task classifier. Respond with ONLY the \
                                  classification in the format: intent|complexity. \
                                  No explanation, no extra text."
                            .into(),
                    },
                    ChatMessage {
                        role: "user".into(),
                        content: prompt,
                    },
                ],
                tools: None,
                max_tokens: Some(20),
                temperature: Some(0.0),
            };

            match registry.chat_completion(request).await {
                Ok(response) => {
                    if let Some(ref content) = response.content {
                        let parsed = parse_slm_classification(content.trim());
                        if let Some(slm_class) = parsed {
                            debug!(
                                intent = ?slm_class.intent,
                                complexity = ?slm_class.complexity,
                                "SLM classifier result"
                            );
                            return Ok(slm_class);
                        }
                        debug!(
                            raw = content,
                            "SLM classifier returned unparseable result, using rule-based"
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "SLM classifier call failed, using rule-based result");
                }
            }
        } else if self.config.agent.classifier.is_some() {
            debug!(
                rule_confidence = classification.confidence,
                "SLM classifier configured but no provider registry available"
            );
        }

        // Level 3: If still uncertain, the orchestrator LLM will decide
        // during routing. Return the best classification we have.
        Ok(classification)
    }

    /// Route a classified task to the best sub-agent.
    pub fn route(
        &self,
        classification: &TaskClassification,
    ) -> RoutingDecision {
        let sub_agents = &self.config.agent.sub_agents;

        // If no sub-agents configured, route to primary
        if sub_agents.is_empty() || matches!(self.config.agent.routing, RoutingStrategy::Single) {
            return self.route_to_primary(classification, "No sub-agents configured");
        }

        // Try to find a matching sub-agent based on strategy
        match &self.config.agent.routing {
            RoutingStrategy::Single => {
                self.route_to_primary(classification, "Single routing mode")
            }
            RoutingStrategy::RuleBased | RoutingStrategy::Hybrid => {
                self.route_by_role(classification, sub_agents)
            }
            RoutingStrategy::LocalFirst => {
                self.route_local_first(classification, sub_agents)
            }
            RoutingStrategy::CostOptimized => {
                self.route_cheapest(classification, sub_agents)
            }
            RoutingStrategy::LlmRouted => {
                // LLM routing would require an actual LLM call to decide.
                // For now, fall back to role-based routing.
                self.route_by_role(classification, sub_agents)
            }
        }
    }

    /// Evaluate a response through the quality gate.
    ///
    /// Supports multiple methods based on config:
    /// - "heuristic" → zero-cost heuristic checks
    /// - "llm" / "slm" → calls an LLM to judge quality
    /// - "auto" → heuristic first, LLM if borderline
    pub async fn evaluate_quality(
        &self,
        task: &TaskClassification,
        response: &ngenorca_plugin_sdk::ChatCompletionResponse,
        original_message: &str,
        registry: &ProviderRegistry,
    ) -> Result<(QualityVerdict, QualityMethod)> {
        use ngenorca_plugin_sdk::QualityGate;

        if !self.config.agent.quality_gate.enabled {
            return Ok((
                QualityVerdict::Accept { score: None },
                QualityMethod::AutoAccept,
            ));
        }

        let method = self.config.agent.quality_gate.method.as_str();

        match method {
            "llm" | "slm" => {
                // Use LLM-based quality evaluation
                let model = self.config.agent.classifier.as_ref()
                    .map(|c| c.model.clone())
                    .unwrap_or_else(|| self.config.agent.model.clone());
                let llm_gate = super::quality::LlmQualityGate::new(model);
                llm_gate.evaluate_with_provider(task, response, original_message, registry).await
            }
            "auto" => {
                // Heuristic first; if borderline (score 0.5–0.7), use LLM
                let (verdict, heur_method) = self.quality_gate
                    .evaluate(task, response, original_message)
                    .await?;

                if let QualityVerdict::Accept { score: Some(s) } = &verdict {
                    if *s >= 0.5 && *s < 0.7 {
                        // Borderline — get a second opinion from LLM
                        let model = self.config.agent.classifier.as_ref()
                            .map(|c| c.model.clone())
                            .unwrap_or_else(|| self.config.agent.model.clone());
                        let llm_gate = super::quality::LlmQualityGate::new(model);
                        return llm_gate
                            .evaluate_with_provider(task, response, original_message, registry)
                            .await;
                    }
                }
                Ok((verdict, heur_method))
            }
            _ => {
                // Default: heuristic-only
                self.quality_gate
                    .evaluate(task, response, original_message)
                    .await
            }
        }
    }

    /// Find the sub-agent to escalate to (more capable than the current one).
    pub fn find_escalation_target(
        &self,
        current_agent: &str,
    ) -> Option<SubAgentId> {
        let current = self.config.sub_agent(current_agent);
        let current_cost = current.map(|a| a.cost_weight).unwrap_or(0);

        // Find a more capable (= more expensive) agent
        let escalation = self
            .config
            .agent
            .sub_agents
            .iter()
            .filter(|a| a.name != current_agent && a.cost_weight > current_cost)
            .min_by_key(|a| a.cost_weight);

        if let Some(agent) = escalation {
            Some(SubAgentId {
                name: agent.name.clone(),
                model: agent.model.clone(),
            })
        } else {
            // Escalate to primary model
            Some(SubAgentId {
                name: "primary".into(),
                model: self.config.agent.model.clone(),
            })
        }
    }

    /// Generate a dynamic system prompt for a sub-agent based on the task.
    pub fn generate_system_prompt(
        &self,
        agent: &SubAgentConfig,
        classification: &TaskClassification,
        user_language: Option<&str>,
    ) -> String {
        let mut prompt = String::new();

        // Base system prompt from config
        if let Some(ref base) = agent.system_prompt {
            prompt.push_str(base);
            prompt.push('\n');
        }

        // Add task-specific instructions
        match &classification.intent {
            TaskIntent::Summarization => {
                prompt.push_str("You are summarizing content. Be concise and capture key points.\n");
            }
            TaskIntent::Translation => {
                prompt.push_str("You are translating text. Preserve meaning and tone.\n");
            }
            TaskIntent::Coding => {
                prompt.push_str("You are generating/reviewing code. Use proper formatting with code blocks. Be precise.\n");
            }
            TaskIntent::Analysis => {
                prompt.push_str("You are analysing information. Be thorough and structured.\n");
            }
            TaskIntent::Creative => {
                prompt.push_str("You are doing creative work. Be original and engaging.\n");
            }
            TaskIntent::Extraction => {
                prompt.push_str("You are extracting structured data. Be precise and complete.\n");
            }
            TaskIntent::Reasoning => {
                prompt.push_str("You are solving a logical/mathematical problem. Show your work.\n");
            }
            TaskIntent::Planning => {
                prompt.push_str("You are creating a plan. Be structured with clear steps.\n");
            }
            TaskIntent::QuestionAnswering => {
                prompt.push_str("Answer directly and concisely.\n");
            }
            _ => {}
        }

        // Language preference
        match user_language {
            Some("pt") => prompt.push_str("Responde em português.\n"),
            Some("es") => prompt.push_str("Responde en español.\n"),
            Some("en") => prompt.push_str("Respond in English.\n"),
            Some(lang) => {
                prompt.push_str(&format!("Respond in {}.\n", lang));
            }
            None => {}
        }

        // Domain context
        if !classification.domain_tags.is_empty() {
            prompt.push_str(&format!(
                "Domain context: {}.\n",
                classification.domain_tags.join(", ")
            ));
        }

        prompt
    }

    /// Build an orchestration record for learning (to be stored in memory).
    pub fn build_record(
        &self,
        classification: TaskClassification,
        routing: RoutingDecision,
        quality: QualityVerdict,
        quality_method: QualityMethod,
        escalated: bool,
        latency_ms: u64,
        total_tokens: usize,
    ) -> OrchestrationRecord {
        OrchestrationRecord {
            classification,
            routing,
            quality,
            quality_method,
            escalated,
            latency_ms,
            total_tokens,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Execute the full orchestration pipeline:
    /// classify → route → delegate → quality-check → (escalate?) → respond.
    ///
    /// If `memory_context` is provided, semantic and episodic memories are
    /// injected into the system prompt for enhanced contextual responses.
    pub async fn process(
        &self,
        message: &str,
        conversation: &[ChatMessage],
        registry: &ProviderRegistry,
        plugins: Option<&PluginRegistry>,
        memory_context: Option<&ngenorca_memory::ContextPack>,
    ) -> Result<(OrchestratedResponse, OrchestrationRecord)> {
        let start = std::time::Instant::now();
        let mut total_usage = Usage::default();

        // Collect tool definitions from plugin registry (if any).
        let tool_defs: Option<Vec<ToolDefinition>> = if let Some(pr) = plugins {
            let defs = pr.tool_definitions().await;
            if defs.is_empty() { None } else { Some(defs) }
        } else {
            None
        };

        // ── Step 1: Classify ──
        let classification = self.classify(message, Some(registry)).await?;
        debug!(
            intent = ?classification.intent,
            complexity = ?classification.complexity,
            confidence = classification.confidence,
            "Task classified"
        );

        // ── Step 2: Route ──
        let routing = self.route(&classification);
        info!(
            agent = routing.target.name,
            model = routing.target.model,
            reason = routing.reason,
            "Routed to sub-agent"
        );

        // ── Step 3: Build messages with system prompt + memory context ──
        let messages = self.build_messages(
            &routing.system_prompt,
            conversation,
            message,
            memory_context,
        );

        let request = ChatCompletionRequest {
            model: routing.target.model.clone(),
            messages: messages.clone(),
            tools: tool_defs.clone(),
            max_tokens: routing.max_tokens,
            temperature: routing.temperature,
        };

        // ── Step 4: Delegate to provider (with tool-call loop) ──
        let response = self
            .call_with_tools(request, registry, plugins, &mut total_usage)
            .await?;

        // ── Step 5: Quality gate ──
        let (verdict, quality_method) = self
            .evaluate_quality(&classification, &response, message, registry)
            .await?;

        let mut escalated = false;
        let mut final_response = response;
        let mut served_by = routing.target.clone();

        match &verdict {
            QualityVerdict::Accept { score } => {
                debug!(score = ?score, "Quality gate: accepted");
            }
            QualityVerdict::Escalate { reason, escalate_to } => {
                warn!(
                    reason = %reason,
                    escalate_to = ?escalate_to,
                    "Quality gate: escalating"
                );
                escalated = true;

                // Find escalation target
                if let Some(escalation_target) =
                    self.find_escalation_target(&routing.target.name)
                {
                    // Re-send to a more capable model
                    let esc_agent = self
                        .config
                        .sub_agent(&escalation_target.name)
                        .cloned();

                    let esc_system_prompt = if let Some(ref agent) = esc_agent {
                        self.generate_system_prompt(
                            agent,
                            &classification,
                            classification.language.as_deref(),
                        )
                    } else {
                        routing.system_prompt.clone()
                    };

                    let esc_messages = self.build_messages(
                        &esc_system_prompt,
                        conversation,
                        message,
                        memory_context,
                    );

                    let esc_request = ChatCompletionRequest {
                        model: escalation_target.model.clone(),
                        messages: esc_messages,
                        tools: tool_defs.clone(),
                        max_tokens: esc_agent.as_ref().map(|a| a.max_tokens),
                        temperature: esc_agent.as_ref().map(|a| a.temperature),
                    };

                    match self
                        .call_with_tools(esc_request, registry, plugins, &mut total_usage)
                        .await
                    {
                        Ok(esc_response) => {
                            final_response = esc_response;
                            served_by = escalation_target;
                        }
                        Err(e) => {
                            warn!(error = %e, "Escalation failed, using original response");
                        }
                    }
                }
            }
            QualityVerdict::Augment { missing, partial_response } => {
                debug!(
                    missing = %missing,
                    "Quality gate: augmenting response"
                );

                // Build an augmentation prompt that includes the original response
                // and asks the model to address the missing aspects.
                let mut aug_messages = messages.clone();
                if !partial_response.is_empty() {
                    aug_messages.push(ChatMessage {
                        role: "assistant".into(),
                        content: partial_response.clone(),
                    });
                }
                aug_messages.push(ChatMessage {
                    role: "user".into(),
                    content: format!(
                        "Your previous response was incomplete. Please expand on: {}. \
                         Incorporate the information from your previous answer and provide \
                         a complete, comprehensive response.",
                        missing
                    ),
                });

                let aug_request = ChatCompletionRequest {
                    model: routing.target.model.clone(),
                    messages: aug_messages,
                    tools: tool_defs.clone(),
                    max_tokens: routing.max_tokens,
                    temperature: routing.temperature,
                };

                match self
                    .call_with_tools(aug_request, registry, plugins, &mut total_usage)
                    .await
                {
                    Ok(aug_response) => {
                        final_response = aug_response;
                    }
                    Err(e) => {
                        warn!(error = %e, "Augmentation failed, using partial response");
                        if !partial_response.is_empty() {
                            final_response = ChatCompletionResponse {
                                content: Some(partial_response.clone()),
                                tool_calls: final_response.tool_calls,
                                usage: final_response.usage,
                            };
                        }
                    }
                }
            }
        }

        let latency_ms = start.elapsed().as_millis() as u64;

        // ── Step 7: Build the orchestration record for analytics / learned routing ──
        let record = self.build_record(
            classification.clone(),
            routing.clone(),
            verdict.clone(),
            quality_method,
            escalated,
            latency_ms,
            total_usage.total_tokens,
        );

        let content = final_response
            .content
            .unwrap_or_else(|| "[No response content]".into());

        Ok((OrchestratedResponse {
            content,
            tool_calls: final_response.tool_calls,
            served_by,
            classification,
            routing,
            quality: verdict,
            escalated,
            total_usage,
            latency_ms,
        }, record))
    }

    /// Build the message array for a chat completion request.
    ///
    /// Also handles the tool-call loop: if the LLM returns tool calls,
    /// execute them via the plugin registry, append results, and re-send.
    async fn call_with_tools(
        &self,
        mut request: ChatCompletionRequest,
        registry: &ProviderRegistry,
        plugins: Option<&PluginRegistry>,
        total_usage: &mut Usage,
    ) -> Result<ChatCompletionResponse> {
        const MAX_TOOL_ROUNDS: usize = 8; // Safety limit to prevent infinite loops

        for round in 0..MAX_TOOL_ROUNDS {
            let response = registry.chat_completion(request.clone()).await?;
            total_usage.prompt_tokens += response.usage.prompt_tokens;
            total_usage.completion_tokens += response.usage.completion_tokens;
            total_usage.total_tokens += response.usage.total_tokens;

            // If no tool calls, we're done.
            if response.tool_calls.is_empty() {
                return Ok(response);
            }

            // If we don't have a plugin registry, return as-is (caller handles tool_calls).
            let Some(pr) = plugins else {
                return Ok(response);
            };

            debug!(
                round = round,
                tool_calls = response.tool_calls.len(),
                "Executing tool calls"
            );

            // Append the assistant message with tool calls to the conversation.
            request.messages.push(ChatMessage {
                role: "assistant".into(),
                content: response.content.clone().unwrap_or_default(),
            });

            // Execute each tool call and append results.
            for tc in &response.tool_calls {
                let result = match pr
                    .execute_tool(
                        &tc.name,
                        tc.arguments.clone(),
                        &ngenorca_core::SessionId::new(), // session context
                        None,
                    )
                    .await
                {
                    Ok(value) => serde_json::to_string(&value).unwrap_or_default(),
                    Err(e) => format!("Tool error: {e}"),
                };

                debug!(
                    tool = %tc.name,
                    call_id = %tc.id,
                    "Tool executed"
                );

                // Append tool result as a "tool" role message.
                request.messages.push(ChatMessage {
                    role: "tool".into(),
                    content: format!(
                        "[Tool: {} (call_id: {})]\n{}",
                        tc.name, tc.id, result
                    ),
                });
            }

            // Next iteration will re-send with the tool results appended.
        }

        // Safety: if we exhausted rounds, return the last response.
        warn!("Tool-call loop hit maximum rounds ({})", MAX_TOOL_ROUNDS);
        registry.chat_completion(request).await
    }

    /// Build the message array for a chat completion request.
    ///
    /// When `memory_context` is provided, semantic facts and episodic
    /// snippets are injected into the system prompt so the LLM can
    /// reference long-term knowledge about the user.
    fn build_messages(
        &self,
        system_prompt: &str,
        conversation: &[ChatMessage],
        current_message: &str,
        memory_context: Option<&ngenorca_memory::ContextPack>,
    ) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // System prompt
        let mut system = if system_prompt.is_empty() {
            self.config
                .agent
                .sub_agents
                .first()
                .and_then(|a| a.system_prompt.clone())
                .unwrap_or_else(|| {
                    "You are NgenOrca, a helpful personal AI assistant.".to_string()
                })
        } else {
            system_prompt.to_string()
        };

        // Inject memory context into the system prompt.
        if let Some(ctx) = memory_context {
            // Semantic facts (long-term knowledge about the user).
            if !ctx.semantic_block.is_empty() {
                system.push_str("\n\n## What you know about this user:\n");
                for fact in &ctx.semantic_block {
                    system.push_str(&format!(
                        "- {} (confidence: {:.0}%)\n",
                        fact.fact,
                        fact.confidence * 100.0,
                    ));
                }
            }

            // Episodic memories (relevant past conversations).
            if !ctx.episodic_snippets.is_empty() {
                system.push_str("\n## Relevant past conversations:\n");
                for ep in &ctx.episodic_snippets {
                    let when = ep.timestamp.format("%Y-%m-%d");
                    let snippet = if ep.content.len() > 200 {
                        format!("{}…", &ep.content[..200])
                    } else {
                        ep.content.clone()
                    };
                    system.push_str(&format!("- [{}] {}\n", when, snippet));
                }
            }
        }

        messages.push(ChatMessage {
            role: "system".into(),
            content: system,
        });

        // Conversation history
        for msg in conversation {
            messages.push(msg.clone());
        }

        // Current user message
        messages.push(ChatMessage {
            role: "user".into(),
            content: current_message.to_string(),
        });

        messages
    }

    /// Get summary info about the orchestration setup.
    pub fn info(&self) -> OrchestratorInfo {
        OrchestratorInfo {
            routing_strategy: format!("{:?}", self.config.agent.routing),
            primary_model: self.config.agent.model.clone(),
            classifier_model: self
                .config
                .agent
                .classifier
                .as_ref()
                .map(|c| c.model.clone()),
            sub_agents: self
                .config
                .agent
                .sub_agents
                .iter()
                .map(|a| SubAgentInfo {
                    name: a.name.clone(),
                    model: a.model.clone(),
                    roles: a.roles.clone(),
                    is_local: a.is_local,
                    cost_weight: a.cost_weight,
                    max_complexity: a.max_complexity.clone(),
                })
                .collect(),
            quality_gate_enabled: self.config.agent.quality_gate.enabled,
            quality_method: self.config.agent.quality_gate.method.clone(),
        }
    }

    // ─── Private routing helpers ────────────────────────────────

    fn route_to_primary(
        &self,
        _classification: &TaskClassification,
        reason: &str,
    ) -> RoutingDecision {
        RoutingDecision {
            target: SubAgentId {
                name: "primary".into(),
                model: self.config.agent.model.clone(),
            },
            reason: reason.into(),
            system_prompt: String::new(), // Primary uses its own prompt
            temperature: None,
            max_tokens: None,
            from_memory: false,
        }
    }

    fn route_by_role(
        &self,
        classification: &TaskClassification,
        sub_agents: &[SubAgentConfig],
    ) -> RoutingDecision {
        let intent_str = intent_to_role_string(&classification.intent);

        // Find agents matching this role, sorted by priority
        let mut matching: Vec<&SubAgentConfig> = sub_agents
            .iter()
            .filter(|a| {
                a.roles.iter().any(|r| r.eq_ignore_ascii_case(&intent_str))
                    && complexity_within_limit(classification.complexity, &a.max_complexity)
            })
            .collect();

        matching.sort_by_key(|a| a.priority);

        if let Some(agent) = matching.first() {
            let system_prompt = self.generate_system_prompt(
                agent,
                classification,
                classification.language.as_deref(),
            );

            info!(
                agent = agent.name,
                model = agent.model,
                intent = ?classification.intent,
                "Routing to sub-agent by role"
            );

            RoutingDecision {
                target: SubAgentId {
                    name: agent.name.clone(),
                    model: agent.model.clone(),
                },
                reason: format!(
                    "Role match: {} → {} (priority {})",
                    intent_str, agent.name, agent.priority
                ),
                system_prompt,
                temperature: Some(agent.temperature),
                max_tokens: Some(agent.max_tokens),
                from_memory: false,
            }
        } else {
            // No matching sub-agent — fall back to primary
            debug!(
                intent = intent_str,
                "No sub-agent matches role, routing to primary"
            );
            self.route_to_primary(classification, &format!("No sub-agent for role: {}", intent_str))
        }
    }

    fn route_local_first(
        &self,
        classification: &TaskClassification,
        sub_agents: &[SubAgentConfig],
    ) -> RoutingDecision {
        let intent_str = intent_to_role_string(&classification.intent);

        // Prefer local agents first
        let local = sub_agents
            .iter()
            .filter(|a| {
                a.is_local
                    && (a.roles.iter().any(|r| r.eq_ignore_ascii_case(&intent_str))
                        || a.roles.iter().any(|r| r.eq_ignore_ascii_case("general")))
                    && complexity_within_limit(classification.complexity, &a.max_complexity)
            })
            .min_by_key(|a| a.priority);

        if let Some(agent) = local {
            let system_prompt = self.generate_system_prompt(
                agent,
                classification,
                classification.language.as_deref(),
            );

            info!(
                agent = agent.name,
                model = agent.model,
                "Routing to local sub-agent (LocalFirst)"
            );

            RoutingDecision {
                target: SubAgentId {
                    name: agent.name.clone(),
                    model: agent.model.clone(),
                },
                reason: format!("LocalFirst: {} (local, priority {})", agent.name, agent.priority),
                system_prompt,
                temperature: Some(agent.temperature),
                max_tokens: Some(agent.max_tokens),
                from_memory: false,
            }
        } else {
            // No local agent fits — route by role (may pick cloud)
            self.route_by_role(classification, sub_agents)
        }
    }

    fn route_cheapest(
        &self,
        classification: &TaskClassification,
        sub_agents: &[SubAgentConfig],
    ) -> RoutingDecision {
        let intent_str = intent_to_role_string(&classification.intent);

        let cheapest = sub_agents
            .iter()
            .filter(|a| {
                (a.roles.iter().any(|r| r.eq_ignore_ascii_case(&intent_str))
                    || a.roles.iter().any(|r| r.eq_ignore_ascii_case("general")))
                    && complexity_within_limit(classification.complexity, &a.max_complexity)
            })
            .min_by_key(|a| a.cost_weight);

        if let Some(agent) = cheapest {
            let system_prompt = self.generate_system_prompt(
                agent,
                classification,
                classification.language.as_deref(),
            );

            info!(
                agent = agent.name,
                model = agent.model,
                cost = agent.cost_weight,
                "Routing to cheapest sub-agent"
            );

            RoutingDecision {
                target: SubAgentId {
                    name: agent.name.clone(),
                    model: agent.model.clone(),
                },
                reason: format!(
                    "CostOptimized: {} (cost_weight {})",
                    agent.name, agent.cost_weight
                ),
                system_prompt,
                temperature: Some(agent.temperature),
                max_tokens: Some(agent.max_tokens),
                from_memory: false,
            }
        } else {
            self.route_to_primary(classification, "No suitable cheap agent found")
        }
    }
}

/// Public info struct for API endpoints.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrchestratorInfo {
    pub routing_strategy: String,
    pub primary_model: String,
    pub classifier_model: Option<String>,
    pub sub_agents: Vec<SubAgentInfo>,
    pub quality_gate_enabled: bool,
    pub quality_method: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubAgentInfo {
    pub name: String,
    pub model: String,
    pub roles: Vec<String>,
    pub is_local: bool,
    pub cost_weight: u32,
    pub max_complexity: String,
}

// ─── Helpers ────────────────────────────────────────────────────

fn intent_to_role_string(intent: &TaskIntent) -> String {
    match intent {
        TaskIntent::Conversation => "General",
        TaskIntent::Summarization => "Summarization",
        TaskIntent::Translation => "Translation",
        TaskIntent::Coding => "Coding",
        TaskIntent::Analysis => "Analysis",
        TaskIntent::Creative => "Creative",
        TaskIntent::QuestionAnswering => "QuestionAnswering",
        TaskIntent::Planning => "Planning",
        TaskIntent::Extraction => "Extraction",
        TaskIntent::Reasoning => "Reasoning",
        TaskIntent::Vision => "Vision",
        TaskIntent::ToolUse => "ToolUse",
        TaskIntent::Unknown => "General",
        TaskIntent::Custom(s) => s.as_str(),
    }
    .to_string()
}

fn complexity_within_limit(actual: TaskComplexity, max_str: &str) -> bool {
    let max = match max_str.to_lowercase().as_str() {
        "trivial" => TaskComplexity::Trivial,
        "simple" => TaskComplexity::Simple,
        "moderate" => TaskComplexity::Moderate,
        "complex" => TaskComplexity::Complex,
        "expert" => TaskComplexity::Expert,
        _ => TaskComplexity::Expert, // Unknown = no limit
    };
    actual <= max
}

/// Parse the output of the SLM classifier.
///
/// Expected format: `intent|complexity` (e.g. `coding|moderate`)
fn parse_slm_classification(raw: &str) -> Option<TaskClassification> {
    // Clean the response — SLMs sometimes add quotes, whitespace, etc.
    let cleaned = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_lowercase();

    let parts: Vec<&str> = cleaned.split('|').collect();
    if parts.len() < 2 {
        return None;
    }

    let intent = match parts[0].trim() {
        "summarization" | "summary" | "summarize" => TaskIntent::Summarization,
        "translation" | "translate" => TaskIntent::Translation,
        "coding" | "code" | "programming" => TaskIntent::Coding,
        "analysis" | "analyze" | "analyse" => TaskIntent::Analysis,
        "creative" | "creativity" | "writing" => TaskIntent::Creative,
        "extraction" | "extract" => TaskIntent::Extraction,
        "reasoning" | "reason" | "math" | "logic" => TaskIntent::Reasoning,
        "planning" | "plan" => TaskIntent::Planning,
        "question_answering" | "qa" | "question" => TaskIntent::QuestionAnswering,
        "unknown" => TaskIntent::Unknown,
        _ => return None,
    };

    let complexity = match parts[1].trim() {
        "trivial" => TaskComplexity::Trivial,
        "simple" => TaskComplexity::Simple,
        "moderate" | "medium" => TaskComplexity::Moderate,
        "complex" | "hard" => TaskComplexity::Complex,
        "expert" => TaskComplexity::Expert,
        _ => TaskComplexity::Simple,
    };

    Some(TaskClassification {
        intent,
        complexity,
        confidence: 0.85, // SLM classifications get a fixed confidence
        method: ClassificationMethod::SlmClassifier,
        domain_tags: vec![],
        language: None, // SLM doesn't detect language in this mode
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_core::orchestration::ClassificationMethod;
    use ngenorca_config::SubAgentConfig;

    fn test_config() -> NgenOrcaConfig {
        let mut config = NgenOrcaConfig::default();
        config.agent.routing = RoutingStrategy::Hybrid;
        config.agent.sub_agents = vec![
            SubAgentConfig {
                name: "local-general".into(),
                model: "ollama/llama3.1:8b".into(),
                roles: vec![
                    "General".into(),
                    "Summarization".into(),
                    "Translation".into(),
                    "QuestionAnswering".into(),
                ],
                system_prompt: Some("You are a helpful assistant.".into()),
                max_tokens: 1024,
                temperature: 0.3,
                max_complexity: "Moderate".into(),
                is_local: true,
                cost_weight: 1,
                priority: 1,
            },
            SubAgentConfig {
                name: "coder".into(),
                model: "ollama/codellama:13b".into(),
                roles: vec!["Coding".into(), "Extraction".into()],
                system_prompt: Some("You are an expert programmer.".into()),
                max_tokens: 4096,
                temperature: 0.2,
                max_complexity: "Complex".into(),
                is_local: true,
                cost_weight: 2,
                priority: 1,
            },
            SubAgentConfig {
                name: "deep-thinker".into(),
                model: "anthropic/claude-sonnet-4-20250514".into(),
                roles: vec![
                    "Analysis".into(),
                    "Planning".into(),
                    "Creative".into(),
                    "Reasoning".into(),
                ],
                system_prompt: None,
                max_tokens: 8192,
                temperature: 0.5,
                max_complexity: "Expert".into(),
                is_local: false,
                cost_weight: 8,
                priority: 10,
            },
        ];
        config
    }

    #[tokio::test]
    async fn test_classify_summarization() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);

        let result = orch.classify("resume este artigo sobre redes", None).await.unwrap();
        assert_eq!(result.intent, TaskIntent::Summarization);
    }

    #[test]
    fn test_route_summarization_to_local() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);

        let classification = TaskClassification {
            intent: TaskIntent::Summarization,
            complexity: TaskComplexity::Simple,
            confidence: 0.9,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: Some("pt".into()),
        };

        let decision = orch.route(&classification);
        assert_eq!(decision.target.name, "local-general");
        assert!(decision.system_prompt.contains("português"));
    }

    #[test]
    fn test_route_coding_to_coder() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);

        let classification = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: TaskComplexity::Moderate,
            confidence: 0.85,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["rust".into()],
            language: Some("en".into()),
        };

        let decision = orch.route(&classification);
        assert_eq!(decision.target.name, "coder");
    }

    #[test]
    fn test_route_analysis_to_deep_thinker() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);

        let classification = TaskClassification {
            intent: TaskIntent::Analysis,
            complexity: TaskComplexity::Complex,
            confidence: 0.8,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: None,
        };

        let decision = orch.route(&classification);
        assert_eq!(decision.target.name, "deep-thinker");
    }

    #[test]
    fn test_escalation_target() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);

        let target = orch.find_escalation_target("local-general").unwrap();
        assert_eq!(target.name, "coder"); // Next cheapest
    }

    #[test]
    fn test_local_first_routing() {
        let mut config = test_config();
        config.agent.routing = RoutingStrategy::LocalFirst;
        let config = Arc::new(config);
        let orch = HybridOrchestrator::new(config);

        let classification = TaskClassification {
            intent: TaskIntent::Analysis,
            complexity: TaskComplexity::Simple,
            confidence: 0.8,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: None,
        };

        let decision = orch.route(&classification);
        // Should pick local-general (is_local=true, has General role) even though
        // deep-thinker has Analysis role, because LocalFirst prefers local
        assert!(decision.target.name == "local-general" || decision.target.name == "coder");
    }

    #[test]
    fn test_info() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let info = orch.info();

        assert_eq!(info.sub_agents.len(), 3);
        assert_eq!(info.routing_strategy, "Hybrid");
    }
}
