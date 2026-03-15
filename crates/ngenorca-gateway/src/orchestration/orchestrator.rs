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

use futures::future::join_all;
use ngenorca_bus::EventBus;
use ngenorca_config::{NgenOrcaConfig, RoutingStrategy, SubAgentConfig};
use ngenorca_core::Result;
use ngenorca_core::orchestration::{
    ClassificationMethod, CorrectionRecord, OrchestrationRecord, QualityMethod, QualityVerdict,
    RoutingDecision, SubAgentId, SynthesisRecord, TaskClassification, TaskComplexity,
    TaskIntent,
};
use ngenorca_core::types::{SessionId, UserId};
use ngenorca_plugin_sdk::{
    BranchEvidenceDiagnostics, ChatCompletionRequest, ChatCompletionResponse, ChatMessage,
    CorrectionAttemptTrace, CorrectionDiagnostics, DelegationPlanDiagnostics,
    DelegationStepDiagnostics, OrchestrationDiagnostics, OrchestratedResponse,
    SynthesisDiagnostics, ToolCallResponse, ToolDefinition, Usage, VerificationDiagnostics,
    WorkerExecutionTrace,
};
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::classifier::RuleBasedClassifier;
use super::quality::HeuristicQualityGate;
use crate::orchestration::LearnedRouter;
use crate::plugins::PluginRegistry;
use crate::providers::ProviderRegistry;

#[derive(Clone, Copy, Default)]
pub struct InvocationContext<'a> {
    pub learned_router: Option<&'a LearnedRouter>,
    pub session_id: Option<&'a SessionId>,
    pub user_id: Option<&'a UserId>,
    pub channel: Option<&'a str>,
    pub event_bus: Option<&'a EventBus>,
}

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

        if classification.confidence >= threshold && classification.intent != TaskIntent::Unknown {
            debug!(
                intent = ?classification.intent,
                confidence = classification.confidence,
                "Rule-based classification accepted (confidence >= {threshold})"
            );
            return Ok(classification);
        }

        // Level 2: SLM classifier (if configured and provider available)
        if let (Some(classifier_cfg), Some(registry)) = (&self.config.agent.classifier, registry) {
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
    pub fn route(&self, classification: &TaskClassification) -> RoutingDecision {
        self.route_with_learned(classification, None)
    }

    /// Route a classified task to the best sub-agent, optionally consulting learned rules.
    pub fn route_with_learned(
        &self,
        classification: &TaskClassification,
        learned_router: Option<&LearnedRouter>,
    ) -> RoutingDecision {
        let sub_agents = &self.config.agent.sub_agents;

        if let Some(router) = learned_router
            && let Some(decision) = self.route_from_learned(classification, router)
        {
            return decision;
        }

        // If no sub-agents configured, route to primary
        if sub_agents.is_empty() || matches!(self.config.agent.routing, RoutingStrategy::Single) {
            return self.route_to_primary(classification, "No sub-agents configured");
        }

        // Try to find a matching sub-agent based on strategy
        match &self.config.agent.routing {
            RoutingStrategy::Single => self.route_to_primary(classification, "Single routing mode"),
            RoutingStrategy::RuleBased | RoutingStrategy::Hybrid => {
                self.route_by_role(classification, sub_agents)
            }
            RoutingStrategy::LocalFirst => self.route_local_first(classification, sub_agents),
            RoutingStrategy::CostOptimized => self.route_cheapest(classification, sub_agents),
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
                let model = self
                    .config
                    .agent
                    .classifier
                    .as_ref()
                    .map(|c| c.model.clone())
                    .unwrap_or_else(|| self.config.agent.model.clone());
                let llm_gate = super::quality::LlmQualityGate::new(model);
                llm_gate
                    .evaluate_with_provider(task, response, original_message, registry)
                    .await
            }
            "auto" => {
                // Heuristic first; if borderline (score 0.5–0.7), use LLM
                let (verdict, heur_method) = self
                    .quality_gate
                    .evaluate(task, response, original_message)
                    .await?;

                if let QualityVerdict::Accept { score: Some(s) } = &verdict
                    && *s >= 0.5
                    && *s < 0.7
                {
                    // Borderline — get a second opinion from LLM
                    let model = self
                        .config
                        .agent
                        .classifier
                        .as_ref()
                        .map(|c| c.model.clone())
                        .unwrap_or_else(|| self.config.agent.model.clone());
                    let llm_gate = super::quality::LlmQualityGate::new(model);
                    return llm_gate
                        .evaluate_with_provider(task, response, original_message, registry)
                        .await;
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
    pub fn find_escalation_target(&self, current_agent: &str) -> Option<SubAgentId> {
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

        prompt.push_str(&worker_contract(agent, classification));
        prompt.push('\n');

        // Base system prompt from config
        if let Some(ref base) = agent.system_prompt {
            prompt.push_str(base);
            prompt.push('\n');
        }

        // Add task-specific instructions
        match &classification.intent {
            TaskIntent::Summarization => {
                prompt
                    .push_str("You are summarizing content. Be concise and capture key points.\n");
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
                prompt
                    .push_str("You are solving a logical/mathematical problem. Show your work.\n");
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
        invocation: InvocationContext<'_>,
    ) -> Result<(OrchestratedResponse, OrchestrationRecord)> {
        let classification = self.classify(message, Some(registry)).await?;
        self.process_with_classification(
            message,
            &classification,
            conversation,
            registry,
            plugins,
            memory_context,
            invocation,
        )
        .await
    }

    pub async fn process_with_classification(
        &self,
        message: &str,
        classification: &TaskClassification,
        conversation: &[ChatMessage],
        registry: &ProviderRegistry,
        plugins: Option<&PluginRegistry>,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        invocation: InvocationContext<'_>,
    ) -> Result<(OrchestratedResponse, OrchestrationRecord)> {
        let start = std::time::Instant::now();
        let mut total_usage = Usage::default();
        let mut specialist_drafts = Vec::new();
        let mut worker_stages = Vec::new();
        let mut verification_attempted = false;
        let mut verification = None;
        let mut remediation_attempted = false;
        let mut remediation_succeeded = false;
        let mut post_synthesis_verification_attempted = false;
        let mut post_synthesis_drift_corrected = false;
        let mut synthesis_diagnostics = SynthesisDiagnostics::default();

        // Collect tool definitions from plugin registry (if any).
        let tool_defs: Option<Vec<ToolDefinition>> = if let Some(pr) = plugins {
            let defs = pr.tool_definitions().await;
            if defs.is_empty() { None } else { Some(defs) }
        } else {
            None
        };

        debug!(
            intent = ?classification.intent,
            complexity = ?classification.complexity,
            confidence = classification.confidence,
            "Task classified"
        );

        // ── Step 2: Route ──
        let routing = self.route_with_learned(classification, invocation.learned_router);
        info!(
            agent = routing.target.name,
            model = routing.target.model,
            reason = routing.reason,
            "Routed to sub-agent"
        );
        let delegation_plan = self.build_delegation_plan(classification, &routing);
        let execution_step = delegation_plan
            .as_ref()
            .and_then(|plan| execution_plan_step(plan, &routing.target));
        let execution_memory_view = execution_step
            .and_then(|step| build_branch_memory_view(step, memory_context));
        let execution_memory_context = execution_memory_view
            .as_ref()
            .map_or(memory_context, |view| Some(&view.context));
        if let Some(view) = execution_memory_view.as_ref() {
            synthesis_diagnostics.memory_slicing_applied = true;
            synthesis_diagnostics
                .branch_evidence
                .push(branch_evidence_diagnostics(
                    &execution_step.expect("execution step should exist when memory view exists").id,
                    &routing.target,
                    "execution",
                    view,
                ));
        }

        // ── Step 3: Build messages with system prompt + memory context ──
        let messages = self.build_messages(
            &routing.system_prompt,
            conversation,
            message,
            tool_defs.as_deref(),
            execution_memory_context,
            delegation_plan.as_ref(),
        );

        let request = ChatCompletionRequest {
            model: routing.target.model.clone(),
            messages: messages.clone(),
            tools: tool_defs.clone(),
            max_tokens: routing.max_tokens,
            temperature: routing.temperature,
        };

        // ── Step 4: Delegate to provider (with tool-call loop) ──
        let (response, mut tool_summary) = if let Some(plan) = delegation_plan.as_ref() {
            let support_steps = parallel_support_steps(plan, &routing.target);

            if support_steps.is_empty() {
                let (response, tool_summary, usage) = self
                    .call_with_tools_collect(request, registry, plugins, invocation)
                    .await?;
                merge_usage(&mut total_usage, &usage);
                (response, tool_summary)
            } else {
                let support_requests = support_steps
                    .iter()
                    .map(|step| {
                        self.build_parallel_support_request(
                            classification,
                            step,
                            conversation,
                            message,
                            memory_context,
                            Some(plan),
                        )
                    })
                    .collect::<Vec<_>>();

                let main_future =
                    self.call_with_tools_collect(request, registry, plugins, invocation);
                let support_future = join_all(support_requests.into_iter().map(
                    |(support_request, memory_view)| async move {
                        (
                            memory_view,
                            self.call_with_tools_collect(support_request, registry, None, invocation)
                                .await,
                        )
                    },
                ));

                let (main_result, support_results) = tokio::join!(main_future, support_future);
                let (response, tool_summary, usage) = main_result?;
                merge_usage(&mut total_usage, &usage);

                for (step, (memory_view, support_result)) in
                    support_steps.into_iter().zip(support_results.into_iter())
                {
                    if let Some(view) = memory_view.as_ref() {
                        synthesis_diagnostics.memory_slicing_applied = true;
                        synthesis_diagnostics.branch_evidence.push(branch_evidence_diagnostics(
                            &step.id,
                            &step.agent,
                            "support",
                            view,
                        ));
                    }
                    match support_result {
                        Ok((support_response, _support_summary, support_usage)) => {
                            merge_usage(&mut total_usage, &support_usage);
                            remember_specialist_draft(
                                &mut specialist_drafts,
                                "parallel-support",
                                &step.agent,
                                support_response.content.as_deref(),
                                Some(format!("Parallel plan step {}: {}", step.id, step.goal)),
                                memory_view.as_ref(),
                            );
                            worker_stages.push(WorkerExecutionTrace {
                                stage: "parallel-support".into(),
                                agent: step.agent.clone(),
                                outcome: "completed".into(),
                                note: Some(format!("Completed parallel plan step {}", step.id)),
                            });
                        }
                        Err(e) => {
                            worker_stages.push(WorkerExecutionTrace {
                                stage: "parallel-support".into(),
                                agent: step.agent.clone(),
                                outcome: "failed".into(),
                                note: Some(e.to_string()),
                            });
                            warn!(error = %e, step = %step.id, agent = %step.agent.name, "Parallel support branch failed, continuing with main worker result");
                        }
                    }
                }

                (response, tool_summary)
            }
        } else {
            let (response, tool_summary, usage) = self
                .call_with_tools_collect(request, registry, plugins, invocation)
                .await?;
            merge_usage(&mut total_usage, &usage);
            (response, tool_summary)
        };

        // ── Step 5: Quality gate ──
        let (verdict, quality_method) = if routing.from_memory
            && self.config.agent.quality_gate.auto_accept_learned
        {
            (
                QualityVerdict::Accept { score: Some(1.0) },
                QualityMethod::AutoAccept,
            )
        } else {
            self.evaluate_quality(classification, &response, message, registry)
                .await?
        };

        let mut escalated = false;
        let mut final_response = response;
        let mut served_by = routing.target.clone();
        remember_specialist_draft(
            &mut specialist_drafts,
            "initial",
            &served_by,
            final_response.content.as_deref(),
            Some(routing.reason.clone()),
            execution_memory_view.as_ref(),
        );
        worker_stages.push(WorkerExecutionTrace {
            stage: "initial".into(),
            agent: served_by.clone(),
            outcome: "completed".into(),
            note: Some(routing.reason.clone()),
        });

        match &verdict {
            QualityVerdict::Accept { score } => {
                debug!(score = ?score, "Quality gate: accepted");
            }
            QualityVerdict::Escalate {
                reason,
                escalate_to,
            } => {
                warn!(
                    reason = %reason,
                    escalate_to = ?escalate_to,
                    "Quality gate: escalating"
                );
                escalated = true;

                // Find escalation target
                if let Some(escalation_target) = self.find_escalation_target(&routing.target.name) {
                    // Re-send to a more capable model
                    let esc_agent = self.config.sub_agent(&escalation_target.name).cloned();

                    let esc_system_prompt = if let Some(ref agent) = esc_agent {
                        self.generate_system_prompt(
                            agent,
                            &classification,
                            classification.language.as_deref(),
                        )
                    } else {
                        routing.system_prompt.clone()
                    };

                    let esc_messages = self.build_escalation_messages(
                        classification,
                        &esc_system_prompt,
                        conversation,
                        message,
                        final_response.content.as_deref().unwrap_or_default(),
                        &routing.target,
                        reason,
                        tool_defs.as_deref(),
                        execution_memory_context,
                        delegation_plan.as_ref(),
                    );

                    let esc_request = ChatCompletionRequest {
                        model: escalation_target.model.clone(),
                        messages: esc_messages,
                        tools: tool_defs.clone(),
                        max_tokens: esc_agent.as_ref().map(|a| a.max_tokens),
                        temperature: esc_agent.as_ref().map(|a| a.temperature),
                    };

                    match self
                        .call_with_tools(esc_request, registry, plugins, &mut total_usage, invocation)
                        .await
                    {
                        Ok((esc_response, esc_summary)) => {
                            remember_specialist_draft(
                                &mut specialist_drafts,
                                "escalation",
                                &escalation_target,
                                esc_response.content.as_deref(),
                                Some(format!("Escalated after quality gate: {reason}")),
                                execution_memory_view.as_ref(),
                            );
                            final_response = esc_response;
                            tool_summary.merge(esc_summary);
                            served_by = escalation_target;
                            worker_stages.push(WorkerExecutionTrace {
                                stage: "escalation".into(),
                                agent: served_by.clone(),
                                outcome: "completed".into(),
                                note: Some(format!("Escalated after quality gate: {reason}")),
                            });
                        }
                        Err(e) => {
                            worker_stages.push(WorkerExecutionTrace {
                                stage: "escalation".into(),
                                agent: escalation_target,
                                outcome: "failed".into(),
                                note: Some(e.to_string()),
                            });
                            warn!(error = %e, "Escalation failed, using original response");
                        }
                    }
                }
            }
            QualityVerdict::Augment {
                missing,
                partial_response,
            } => {
                debug!(
                    missing = %missing,
                    "Quality gate: augmenting response"
                );

                // Build an augmentation prompt that includes the original response
                // and asks the model to address the missing aspects.
                let aug_messages = self.build_augmentation_messages(
                    classification,
                    &routing.system_prompt,
                    conversation,
                    message,
                    partial_response,
                    missing,
                    &routing.target,
                    tool_defs.as_deref(),
                    execution_memory_context,
                    delegation_plan.as_ref(),
                );

                let aug_request = ChatCompletionRequest {
                    model: routing.target.model.clone(),
                    messages: aug_messages,
                    tools: tool_defs.clone(),
                    max_tokens: routing.max_tokens,
                    temperature: routing.temperature,
                };

                match self
                    .call_with_tools(aug_request, registry, plugins, &mut total_usage, invocation)
                    .await
                {
                    Ok((aug_response, aug_summary)) => {
                        remember_specialist_draft(
                            &mut specialist_drafts,
                            "augmentation",
                            &routing.target,
                            aug_response.content.as_deref(),
                            Some(format!("Augmented to address missing coverage: {missing}")),
                            execution_memory_view.as_ref(),
                        );
                        final_response = aug_response;
                        tool_summary.merge(aug_summary);
                        worker_stages.push(WorkerExecutionTrace {
                            stage: "augmentation".into(),
                            agent: routing.target.clone(),
                            outcome: "completed".into(),
                            note: Some(format!("Augmented to address missing coverage: {missing}")),
                        });
                    }
                    Err(e) => {
                        worker_stages.push(WorkerExecutionTrace {
                            stage: "augmentation".into(),
                            agent: routing.target.clone(),
                            outcome: "failed".into(),
                            note: Some(e.to_string()),
                        });
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

        if tool_summary.used_tools() {
            const MAX_REMEDIATION_PASSES: usize = 3;
            let mut remediation_passes = 0;
            verification_attempted = true;

            loop {
                match self
                    .verify_tool_grounding(
                        classification,
                        conversation,
                        message,
                        &final_response,
                        &tool_summary,
                        registry,
                        &mut total_usage,
                        execution_memory_context,
                    )
                    .await
                {
                    Ok(report) => {
                        verification = Some(verification_diagnostics(&report));
                        final_response = tool_verified_response(&final_response, &report);

                        let can_retry_tools = report.should_retry_tools
                            && tool_defs.is_some()
                            && plugins.is_some();
                        if !can_retry_tools {
                            break;
                        }

                        if remediation_passes >= MAX_REMEDIATION_PASSES
                            || tool_summary.should_abandon_tool_retries()
                        {
                            if let Some(details) = verification.as_mut() {
                                details.should_retry_tools = false;
                                push_unique(
                                    &mut details.issues,
                                    "automatic remediation stopped after repeated failure signals; returning the best corrected answer with explicit limits".into(),
                                );
                            }
                            break;
                        }

                        remediation_attempted = true;
                        remediation_passes += 1;
                        let response_before_remediation = final_response.content.clone();
                        let rounds_before_remediation = tool_summary.rounds;

                        match self
                            .attempt_tool_remediation(
                                classification,
                                conversation,
                                message,
                                &final_response,
                                &tool_summary,
                                &report,
                                tool_defs.as_deref(),
                                registry,
                                plugins,
                                &mut total_usage,
                                execution_memory_context,
                                invocation,
                            )
                            .await
                        {
                            Ok((remediated, remediation_summary)) => {
                                let next_content = remediated.content.clone();
                                let progress_made = remediation_summary.rounds > 0
                                    || response_before_remediation.as_deref().map(str::trim)
                                        != next_content.as_deref().map(str::trim);
                                final_response = remediated;
                                tool_summary.merge(remediation_summary);

                                if !progress_made && tool_summary.rounds == rounds_before_remediation {
                                    if let Some(details) = verification.as_mut() {
                                        details.should_retry_tools = false;
                                        push_unique(
                                            &mut details.issues,
                                            "automatic remediation made no further progress; returning the best corrected answer with explicit limits".into(),
                                        );
                                    }
                                    break;
                                }
                            }
                            Err(e) => {
                                if let Some(details) = verification.as_mut() {
                                    push_unique(
                                        &mut details.issues,
                                        format!("automatic remediation attempt failed: {e}"),
                                    );
                                    details.should_retry_tools = false;
                                }
                                warn!(error = %e, "Automatic tool remediation attempt failed, using verified response");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        verification = Some(VerificationDiagnostics {
                            grounded: false,
                            should_retry_tools: tool_summary.requires_follow_up_verification(),
                            issues: vec![format!("verification pass failed: {e}")],
                            retry_instruction: tool_summary.retry_instruction(),
                        });
                        warn!(error = %e, "Tool verification pass failed, using pre-verification response");
                        break;
                    }
                }
            }

            remediation_succeeded = remediation_attempted
                && verification
                    .as_ref()
                    .is_some_and(|details| details.grounded && !details.should_retry_tools);
        }

        if synthesis_diagnostics.memory_slicing_applied {
            synthesis_diagnostics.reconciliation_strategy =
                Some("weighted_branch_evidence".into());
        }

        let contradiction_scan = contradiction_scan(&specialist_drafts);
        synthesis_diagnostics.contradiction_score = contradiction_scan.score;
        synthesis_diagnostics.conflicting_branches = contradiction_scan.conflicting_branches;
        synthesis_diagnostics.contradiction_anchor_stage = contradiction_scan.anchor_stage.clone();
        synthesis_diagnostics.conflict_summary = contradiction_scan.summary.clone();
        synthesis_diagnostics.contradiction_signals = contradiction_scan.signals.clone();
        if synthesis_diagnostics.reconciliation_strategy.is_none()
            && contradiction_scan.conflicting_branches > 0
        {
            synthesis_diagnostics.reconciliation_strategy = Some("weighted_branch_evidence_with_conflict_scan".into());
        }

        if should_primary_synthesize(&served_by) {
            synthesis_diagnostics.attempted = true;
            match self
                .synthesize_with_primary(
                    classification,
                    conversation,
                    message,
                    &specialist_drafts,
                    &final_response,
                    registry,
                    &mut total_usage,
                    memory_context,
                    delegation_plan.as_ref(),
                )
                .await
            {
                Ok(synthesized) => {
                    final_response = synthesized;
                    served_by = SubAgentId {
                        name: "primary".into(),
                        model: self.config.agent.model.clone(),
                    };
                    synthesis_diagnostics.succeeded = true;
                    synthesis_diagnostics.used_primary = true;
                }
                Err(e) => {
                    synthesis_diagnostics.fallback_to_worker = true;
                    warn!(error = %e, worker = %served_by.name, "Primary synthesis failed, using worker response directly");
                }
            }
        }

        if tool_summary.used_tools() && synthesis_diagnostics.succeeded {
            post_synthesis_verification_attempted = true;
            let content_before_post_synthesis = final_response.content.clone();
            match self
                .verify_tool_grounding(
                    classification,
                    conversation,
                    message,
                    &final_response,
                    &tool_summary,
                    registry,
                    &mut total_usage,
                    execution_memory_context,
                )
                .await
            {
                Ok(report) => {
                    verification = Some(verification_diagnostics(&report));
                    final_response = tool_verified_response(&final_response, &report);
                    post_synthesis_drift_corrected = content_before_post_synthesis
                        .as_deref()
                        .map(str::trim)
                        != final_response.content.as_deref().map(str::trim);
                }
                Err(e) => {
                    verification = Some(VerificationDiagnostics {
                        grounded: false,
                        should_retry_tools: tool_summary.requires_follow_up_verification(),
                        issues: vec![format!(
                            "post-synthesis verification failed: {e}"
                        )],
                        retry_instruction: tool_summary.retry_instruction(),
                    });
                    warn!(error = %e, "Post-synthesis verification failed, using synthesized response");
                }
            }
        }

        if verification_attempted || post_synthesis_verification_attempted {
            let verification_note = verification.as_ref().map(|details| {
                if details.grounded {
                    if post_synthesis_drift_corrected {
                        "Tool-grounded after final post-synthesis verification corrected drift.".to_string()
                    } else if post_synthesis_verification_attempted {
                        "Tool-grounded after final post-synthesis verification.".to_string()
                    } else {
                        "Tool-grounded after verification.".to_string()
                    }
                } else if let Some(issue) = details.issues.first() {
                    format!("Verification caution: {issue}")
                } else {
                    "Verification completed with unresolved caution.".to_string()
                }
            });
            remember_specialist_draft(
                &mut specialist_drafts,
                "verified",
                &served_by,
                final_response.content.as_deref(),
                verification_note,
                execution_memory_view.as_ref(),
            );
        }

        let latency_ms = start.elapsed().as_millis() as u64;

        // ── Step 7: Build the orchestration record for analytics / learned routing ──
        let record = OrchestrationRecord {
            classification: classification.clone(),
            routing: routing.clone(),
            quality: verdict.clone(),
            quality_method,
            escalated,
            user_id: invocation.user_id.cloned(),
            channel: invocation.channel.map(str::to_string),
            latency_ms,
            total_tokens: total_usage.total_tokens,
            correction: CorrectionRecord {
                tool_rounds: tool_summary.rounds,
                had_failures: tool_summary.had_failures,
                had_blocked_calls: tool_summary.had_blocked_calls,
                verification_attempted,
                grounded: verification.as_ref().is_some_and(|details| details.grounded),
                remediation_attempted,
                remediation_succeeded,
                post_synthesis_verification_attempted,
                post_synthesis_drift_corrected,
            },
            synthesis: SynthesisRecord {
                attempted: synthesis_diagnostics.attempted,
                succeeded: synthesis_diagnostics.succeeded,
                contradiction_score: synthesis_diagnostics.contradiction_score,
                conflicting_branches: synthesis_diagnostics.conflicting_branches,
            },
            timestamp: chrono::Utc::now(),
        };

        let content = final_response
            .content
            .unwrap_or_else(|| "[No response content]".into());

        Ok((
            OrchestratedResponse {
                content,
                tool_calls: final_response.tool_calls,
                served_by,
                classification: classification.clone(),
                routing,
                quality: verdict,
                escalated,
                total_usage,
                latency_ms,
                diagnostics: OrchestrationDiagnostics {
                    plan: delegation_plan.as_ref().map(delegation_plan_diagnostics),
                    worker_stages,
                    correction: CorrectionDiagnostics {
                        tool_rounds: tool_summary.rounds,
                        tools_used: tool_summary.tool_names.clone(),
                        had_failures: tool_summary.had_failures,
                        had_blocked_calls: tool_summary.had_blocked_calls,
                        verification_attempted,
                        verification,
                        remediation_attempted,
                        remediation_succeeded,
                        post_synthesis_verification_attempted,
                        post_synthesis_drift_corrected,
                        attempt_trace: tool_summary.attempt_trace.clone(),
                    },
                    synthesis: synthesis_diagnostics,
                },
            },
            record,
        ))
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
        invocation: InvocationContext<'_>,
    ) -> Result<(ChatCompletionResponse, ToolLoopSummary)> {
        const MAX_TOOL_ROUNDS: usize = 8; // Safety limit to prevent infinite loops
        let mut identical_tool_calls = std::collections::HashMap::<String, usize>::new();
        let mut summary = ToolLoopSummary::default();

        for round in 0..MAX_TOOL_ROUNDS {
            let response = registry.chat_completion(request.clone()).await?;
            total_usage.prompt_tokens += response.usage.prompt_tokens;
            total_usage.completion_tokens += response.usage.completion_tokens;
            total_usage.total_tokens += response.usage.total_tokens;

            // If no tool calls, we're done.
            if response.tool_calls.is_empty() {
                return Ok((response, summary));
            }

            // If we don't have a plugin registry, return as-is (caller handles tool_calls).
            let Some(pr) = plugins else {
                return Ok((response, summary));
            };

            debug!(
                round = round,
                tool_calls = response.tool_calls.len(),
                "Executing tool calls"
            );
            summary.rounds += 1;

            // Append the assistant message with tool calls to the conversation.
            request.messages.push(ChatMessage {
                role: "assistant".into(),
                content: response.content.clone().unwrap_or_default(),
            });

            // Execute each tool call and append results.
            let mut had_tool_issue = false;
            for tc in &response.tool_calls {
                summary.tool_names.push(tc.name.clone());
                let started_at = chrono::Utc::now();
                let exec_start = std::time::Instant::now();
                let fallback_session = invocation.session_id.cloned().unwrap_or_else(SessionId::new);
                let call_signature = tool_call_signature(tc);
                let call_count = identical_tool_calls
                    .entry(call_signature)
                    .and_modify(|count| *count += 1)
                    .or_insert(1);

                let outcome = if *call_count > 1 {
                    ToolExecutionOutcome::Blocked {
                        error: format!(
                            "Repeated identical tool call blocked for '{}' to prevent infinite retry loops. Change the arguments or continue without retrying the same call.",
                            tc.name
                        ),
                    }
                } else {
                    match pr
                        .execute_tool(
                            &tc.name,
                            tc.arguments.clone(),
                            &fallback_session,
                            invocation.user_id,
                        )
                        .await
                    {
                        Ok(value) => ToolExecutionOutcome::Success { value },
                        Err(e) => ToolExecutionOutcome::Failed {
                            error: e.to_string(),
                        },
                    }
                };

                had_tool_issue |= !outcome.is_success();
                summary.had_failures |= matches!(outcome, ToolExecutionOutcome::Failed { .. });
                summary.had_blocked_calls |= matches!(outcome, ToolExecutionOutcome::Blocked { .. });
                let guidance = summary.remember_retry_guidance(&tc.name, &outcome);
                summary
                    .tool_observations
                    .push(ToolObservation::from_outcome(&tc.name, &outcome));
                summary.attempt_trace.push(CorrectionAttemptTrace {
                    round: round + 1,
                    tool: tc.name.clone(),
                    outcome: outcome.label().into(),
                    failure_class: outcome.failure_class(&tc.name),
                    guidance,
                });

                if let Some(event_bus) = invocation.event_bus {
                    let event = ngenorca_core::event::Event {
                        id: ngenorca_core::types::EventId::new(),
                        timestamp: chrono::Utc::now(),
                        session_id: Some(fallback_session.clone()),
                        user_id: invocation.user_id.cloned(),
                        payload: ngenorca_core::event::EventPayload::ToolExecution {
                            tool_name: tc.name.clone(),
                            session_id: fallback_session.clone(),
                            channel: invocation.channel.map(str::to_owned),
                            started_at,
                            duration_ms: Some(exec_start.elapsed().as_millis() as u64),
                            success: Some(outcome.is_success()),
                            failure_class: outcome.failure_class(&tc.name),
                            outcome: Some(outcome.label().into()),
                        },
                    };
                    if let Err(e) = event_bus.publish(event).await {
                        warn!(error = %e, tool = %tc.name, "Failed to publish tool execution event");
                    }
                }

                debug!(
                    tool = %tc.name,
                    call_id = %tc.id,
                    "Tool executed"
                );

                // Append tool result as a "tool" role message.
                let feedback_message = ChatMessage {
                    role: "tool".into(),
                    content: format_tool_feedback(tc, &outcome),
                };
                summary.tool_feedback.push(feedback_message.clone());
                request.messages.push(feedback_message);
            }

            if had_tool_issue {
                let retry_guidance = summary.retry_instruction().unwrap_or_default();
                request.messages.push(ChatMessage {
                    role: "system".into(),
                    content: format!(
                        "One or more tool calls failed or were blocked. Do not repeat the same failing call with identical arguments. Either fix the arguments and retry once, choose a different tool, or answer with the best available result and state the limitation plainly.\n\n{}",
                        retry_guidance
                    ),
                });
            } else if let Some(verification_instruction) = summary.follow_up_verification_instruction() {
                request.messages.push(ChatMessage {
                    role: "system".into(),
                    content: verification_instruction,
                });
            }

            // Next iteration will re-send with the tool results appended.
        }

        // Safety: if we exhausted rounds, return the last response.
        warn!("Tool-call loop hit maximum rounds ({})", MAX_TOOL_ROUNDS);
        let response = registry.chat_completion(request).await?;
        Ok((response, summary))
    }

    async fn verify_tool_grounding(
        &self,
        classification: &TaskClassification,
        conversation: &[ChatMessage],
        current_message: &str,
        draft_response: &ChatCompletionResponse,
        tool_summary: &ToolLoopSummary,
        registry: &ProviderRegistry,
        total_usage: &mut Usage,
        memory_context: Option<&ngenorca_memory::ContextPack>,
    ) -> Result<ToolVerificationReport> {
        let messages = self.build_tool_verification_messages(
            classification,
            conversation,
            current_message,
            draft_response.content.as_deref().unwrap_or_default(),
            tool_summary,
            memory_context,
        );

        let request = ChatCompletionRequest {
            model: self.config.agent.model.clone(),
            messages,
            tools: None,
            max_tokens: None,
            temperature: Some(0.1),
        };

        let response = registry.chat_completion(request).await?;
        total_usage.prompt_tokens += response.usage.prompt_tokens;
        total_usage.completion_tokens += response.usage.completion_tokens;
        total_usage.total_tokens += response.usage.total_tokens;

        Ok(parse_tool_verification_report(
            response.content.as_deref(),
            draft_response.content.as_deref().unwrap_or_default(),
            tool_summary,
        ))
    }

    async fn attempt_tool_remediation(
        &self,
        classification: &TaskClassification,
        conversation: &[ChatMessage],
        current_message: &str,
        draft_response: &ChatCompletionResponse,
        tool_summary: &ToolLoopSummary,
        verification_report: &ToolVerificationReport,
        tool_defs: Option<&[ToolDefinition]>,
        registry: &ProviderRegistry,
        plugins: Option<&PluginRegistry>,
        total_usage: &mut Usage,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        invocation: InvocationContext<'_>,
    ) -> Result<(ChatCompletionResponse, ToolLoopSummary)> {
        let messages = self.build_tool_remediation_messages(
            classification,
            conversation,
            current_message,
            draft_response.content.as_deref().unwrap_or_default(),
            tool_summary,
            verification_report,
            tool_defs,
            memory_context,
        );

        let request = ChatCompletionRequest {
            model: self.config.agent.model.clone(),
            messages,
            tools: tool_defs.map(|defs| defs.to_vec()),
            max_tokens: None,
            temperature: Some(0.1),
        };

        self.call_with_tools(request, registry, plugins, total_usage, invocation)
            .await
    }

    async fn call_with_tools_collect(
        &self,
        request: ChatCompletionRequest,
        registry: &ProviderRegistry,
        plugins: Option<&PluginRegistry>,
        invocation: InvocationContext<'_>,
    ) -> Result<(ChatCompletionResponse, ToolLoopSummary, Usage)> {
        let mut usage = Usage::default();
        let (response, tool_summary) = self
            .call_with_tools(request, registry, plugins, &mut usage, invocation)
            .await?;
        Ok((response, tool_summary, usage))
    }

    async fn synthesize_with_primary(
        &self,
        classification: &TaskClassification,
        conversation: &[ChatMessage],
        current_message: &str,
        specialist_drafts: &[SpecialistDraft],
        worker_response: &ChatCompletionResponse,
        registry: &ProviderRegistry,
        total_usage: &mut Usage,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        delegation_plan: Option<&DelegationPlan>,
    ) -> Result<ChatCompletionResponse> {
        let synthesis_messages = self.build_synthesis_messages(
            classification,
            conversation,
            current_message,
            specialist_drafts,
            worker_response.content.as_deref().unwrap_or_default(),
            memory_context,
            delegation_plan,
        );

        let request = ChatCompletionRequest {
            model: self.config.agent.model.clone(),
            messages: synthesis_messages,
            tools: None,
            max_tokens: None,
            temperature: Some(0.2),
        };

        let response = registry.chat_completion(request).await?;
        total_usage.prompt_tokens += response.usage.prompt_tokens;
        total_usage.completion_tokens += response.usage.completion_tokens;
        total_usage.total_tokens += response.usage.total_tokens;

        Ok(ChatCompletionResponse {
            content: response.content.or_else(|| worker_response.content.clone()),
            tool_calls: vec![],
            usage: response.usage,
        })
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
        tool_defs: Option<&[ToolDefinition]>,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        delegation_plan: Option<&DelegationPlan>,
    ) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // System prompt
        let mut system = self.default_system_prompt(tool_defs);

        if !system_prompt.is_empty() {
            system.push_str("\n\n## Current role instructions:\n");
            system.push_str(system_prompt);
        }

        if let Some(plan) = delegation_plan {
            system.push_str("\n\n## Structured execution plan:\n");
            system.push_str(&render_delegation_plan(plan));
        }

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

            if !ctx.working_messages.is_empty() {
                system.push_str("\n## Active working context:\n");
                for message in &ctx.working_messages {
                    system.push_str(&format!(
                        "- {}: {}\n",
                        message.role,
                        trim_snippet(&message.content, 160),
                    ));
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

    fn build_parallel_support_request(
        &self,
        classification: &TaskClassification,
        step: &DelegationPlanStep,
        conversation: &[ChatMessage],
        current_message: &str,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        delegation_plan: Option<&DelegationPlan>,
    ) -> (ChatCompletionRequest, Option<BranchMemoryView>) {
        let system_prompt = self
            .config
            .sub_agent(&step.agent.name)
            .map(|agent| {
                self.generate_system_prompt(agent, classification, classification.language.as_deref())
            })
            .unwrap_or_default();
        let memory_view = build_branch_memory_view(step, memory_context);

        let mut messages = self.build_messages(
            &system_prompt,
            conversation,
            current_message,
            None,
            memory_view.as_ref().map(|view| &view.context),
            delegation_plan,
        );

        messages.push(ChatMessage {
            role: "system".into(),
            content: format!(
                concat!(
                    "You are responsible only for one parallel support branch of the current structured plan. ",
                    "Complete only the named step, return concise specialist findings or a working draft for synthesis, ",
                    "and do not present this as the final user answer. Do not mention internal orchestration or other hidden workers.\n\n",
                    "Step id: {}\n",
                    "Step goal: {}\n",
                    "Task intent: {:?}."
                ),
                step.id,
                step.goal,
                classification.intent,
            ),
        });
        if let Some(view) = memory_view.as_ref() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: format!(
                    "Branch memory scope: {}\nEvidence focus: {}\nEvidence slice:\n- {}",
                    view.memory_scope,
                    view.evidence_focus,
                    view.evidence_items.join("\n- "),
                ),
            });
        }
        messages.push(ChatMessage {
            role: "user".into(),
            content: format!(
                "Complete only the `{}` step for this request. Return the findings or draft needed by the rest of the plan, not the final user-facing answer.",
                step.id
            ),
        });

        (
            ChatCompletionRequest {
                model: step.agent.model.clone(),
                messages,
                tools: None,
                max_tokens: None,
                temperature: Some(0.2),
            },
            memory_view,
        )
    }

    fn build_synthesis_messages(
        &self,
        classification: &TaskClassification,
        conversation: &[ChatMessage],
        current_message: &str,
        specialist_drafts: &[SpecialistDraft],
        worker_response: &str,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        delegation_plan: Option<&DelegationPlan>,
    ) -> Vec<ChatMessage> {
        let mut messages =
            self.build_messages("", conversation, current_message, None, memory_context, delegation_plan);
        let draft_history = specialist_draft_history(specialist_drafts, worker_response);
        let branch_policy = specialist_branch_policy_summary(specialist_drafts);
        let contradiction_scan = contradiction_scan(specialist_drafts);

        let synthesis_instruction = format!(
            concat!(
                "You are now preparing the final answer as NgenOrca, the user's primary assistant. ",
                "One or more delegated specialists may already have done domain work in sequential stages. ",
                "Your job is to produce the final user-facing response in your own voice without mentioning delegation, routing, hidden workers, or internal orchestration.\n\n",
                "Reconcile the specialist drafts into one coherent answer: preserve the useful technical substance, remove internal-only phrasing, and make sure the answer is directly actionable. ",
                "Prefer the latest corrected draft when it fixes earlier issues, but carry forward earlier concrete details if they remain consistent with the latest draft. ",
                "Grounded execution drafts outrank advisory support branches, and advisory branches should mainly shape caveats, risks, or cross-checks unless they clearly confirm the execution branch. ",
                "Use each branch's evidence slice to understand what context that branch actually saw before promoting or discarding its claims. ",
                "If the specialist response contains uncertainty or limitations, keep them but phrase them clearly for the user.\n\n",
                "Task intent: {:?}.\n",
                "Domain tags: {}"
            ),
            classification.intent,
            if classification.domain_tags.is_empty() {
                "none".to_string()
            } else {
                classification.domain_tags.join(", ")
            }
        );

        if !draft_history.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: format!(
                    "Specialist draft history for reconciliation:\n\n{}",
                    draft_history
                ),
            });
        }

        if !branch_policy.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: format!("Branch reconciliation policy:\n\n{}", branch_policy),
            });
        }

        if contradiction_scan.conflicting_branches > 0 {
            messages.push(ChatMessage {
                role: "system".into(),
                content: format!(
                    "Branch contradiction scan:\n- anchor_stage: {}\n- conflict_score: {:.2}\n- conflicting_branches: {}\n- signals: {}\n- summary:\n- {}",
                    contradiction_scan
                        .anchor_stage
                        .as_deref()
                        .unwrap_or("none"),
                    contradiction_scan.score,
                    contradiction_scan.conflicting_branches,
                    if contradiction_scan.signals.is_empty() {
                        "none".to_string()
                    } else {
                        contradiction_scan.signals.join(", ")
                    },
                    contradiction_scan.summary.join("\n- ")
                ),
            });
        }

        messages.push(ChatMessage {
            role: "assistant".into(),
            content: worker_response.to_string(),
        });
        messages.push(ChatMessage {
            role: "system".into(),
            content: synthesis_instruction,
        });
        messages.push(ChatMessage {
            role: "user".into(),
            content: "Rewrite the specialist material into the final answer you will send to the user. Reconcile any differences, keep ownership as NgenOrca, and do not mention internal delegation.".into(),
        });

        messages
    }

    fn build_escalation_messages(
        &self,
        classification: &TaskClassification,
        system_prompt: &str,
        conversation: &[ChatMessage],
        current_message: &str,
        prior_response: &str,
        prior_agent: &SubAgentId,
        escalation_reason: &str,
        tool_defs: Option<&[ToolDefinition]>,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        delegation_plan: Option<&DelegationPlan>,
    ) -> Vec<ChatMessage> {
        let mut messages = self.build_messages(
            system_prompt,
            conversation,
            current_message,
            tool_defs,
            memory_context,
            delegation_plan,
        );

        if !prior_response.trim().is_empty() {
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: prior_response.to_string(),
            });
        }

        messages.push(ChatMessage {
            role: "system".into(),
            content: format!(
                concat!(
                    "You are taking over from a previous specialist draft. ",
                    "Improve it without mentioning the handoff. ",
                    "Address the quality issue directly, keep any correct details that remain useful, and replace weak or incomplete sections with a stronger answer.\n\n",
                    "Previous specialist: {}/{}.\n",
                    "Quality gate reason: {}.\n",
                    "Task intent: {:?}."
                ),
                prior_agent.name,
                prior_agent.model,
                escalation_reason,
                classification.intent,
            ),
        });
        messages.push(ChatMessage {
            role: "user".into(),
            content: "Produce the improved specialist draft that resolves the issue above while preserving any still-correct substance from the earlier draft.".into(),
        });

        messages
    }

    fn build_augmentation_messages(
        &self,
        classification: &TaskClassification,
        system_prompt: &str,
        conversation: &[ChatMessage],
        current_message: &str,
        partial_response: &str,
        missing: &str,
        prior_agent: &SubAgentId,
        tool_defs: Option<&[ToolDefinition]>,
        memory_context: Option<&ngenorca_memory::ContextPack>,
        delegation_plan: Option<&DelegationPlan>,
    ) -> Vec<ChatMessage> {
        let mut messages = self.build_messages(
            system_prompt,
            conversation,
            current_message,
            tool_defs,
            memory_context,
            delegation_plan,
        );

        if !partial_response.trim().is_empty() {
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: partial_response.to_string(),
            });
        }

        messages.push(ChatMessage {
            role: "system".into(),
            content: format!(
                concat!(
                    "You are revising your own earlier specialist draft to close a gap. ",
                    "Keep the parts that are already correct, but expand or repair the answer so the missing coverage is fully addressed.\n\n",
                    "Specialist: {}/{}.\n",
                    "Missing coverage: {}.\n",
                    "Task intent: {:?}."
                ),
                prior_agent.name,
                prior_agent.model,
                missing,
                classification.intent,
            ),
        });
        messages.push(ChatMessage {
            role: "user".into(),
            content: format!(
                "Revise the previous draft so it fully covers: {}. Keep the useful parts and return a complete specialist draft.",
                missing
            ),
        });

        messages
    }

    fn build_tool_verification_messages(
        &self,
        classification: &TaskClassification,
        conversation: &[ChatMessage],
        current_message: &str,
        draft_response: &str,
        tool_summary: &ToolLoopSummary,
        memory_context: Option<&ngenorca_memory::ContextPack>,
    ) -> Vec<ChatMessage> {
        let mut messages = self.build_messages("", conversation, current_message, None, memory_context, None);
        let tool_report = serde_json::json!({
            "tools_used": tool_summary.tool_names,
            "had_failures": tool_summary.had_failures,
            "had_blocked_calls": tool_summary.had_blocked_calls,
            "retry_guidance": tool_summary.retry_guidance,
            "default_issues": tool_summary.default_verification_issues(),
            "needs_command_verification": tool_summary.needs_command_verification(),
            "latest_command_failure": tool_summary.latest_command_failure_issue(),
            "repeated_failure_tools": tool_summary.repeated_failure_tools(),
            "verification_hints": tool_summary.verification_hints(),
            "tool_feedback": tool_summary
                .tool_feedback
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>(),
        });

        messages.push(ChatMessage {
            role: "assistant".into(),
            content: draft_response.to_string(),
        });
        messages.push(ChatMessage {
            role: "system".into(),
            content: format!(
                concat!(
                    "Verify that the draft answer is grounded in the observed tool results before it is sent to the user. ",
                    "If the draft over-claims, contradicts the tool evidence, or hides tool failures, rewrite it into a corrected answer. ",
                    "If the draft is supported, keep it concise and strengthen clarity only. ",
                    "Return JSON only with the shape {{\"grounded\":bool,\"should_retry_tools\":bool,\"corrected_answer\":string,\"retry_instruction\":string|null,\"issues\":string[]}}. ",
                    "Set `should_retry_tools` to true only when one more targeted tool pass is likely to materially improve correctness, such as unresolved tool failures or a missing post-write readback. ",
                    "Never mention internal verification steps.\n\n",
                    "Task intent: {:?}.\n",
                    "Tool verification report:\n{}"
                ),
                classification.intent,
                serde_json::to_string_pretty(&tool_report).unwrap_or_else(|_| tool_report.to_string())
            ),
        });
        messages.push(ChatMessage {
            role: "user".into(),
            content: "Return the verification JSON now. The `corrected_answer` must contain the exact user-facing answer to send if no further tool retry is needed.".into(),
        });

        messages
    }

    fn build_tool_remediation_messages(
        &self,
        classification: &TaskClassification,
        conversation: &[ChatMessage],
        current_message: &str,
        draft_response: &str,
        tool_summary: &ToolLoopSummary,
        verification_report: &ToolVerificationReport,
        tool_defs: Option<&[ToolDefinition]>,
        memory_context: Option<&ngenorca_memory::ContextPack>,
    ) -> Vec<ChatMessage> {
        let mut messages = self.build_messages(
            "",
            conversation,
            current_message,
            tool_defs,
            memory_context,
            None,
        );

        let remediation_report = serde_json::json!({
            "grounded": verification_report.grounded,
            "issues": verification_report.issues,
            "retry_instruction": verification_report.retry_instruction,
            "retry_guidance": tool_summary.retry_guidance,
            "default_issues": tool_summary.default_verification_issues(),
            "needs_command_verification": tool_summary.needs_command_verification(),
            "latest_command_failure": tool_summary.latest_command_failure_issue(),
            "repeated_failure_tools": tool_summary.repeated_failure_tools(),
            "verification_hints": tool_summary.verification_hints(),
            "needs_write_verification": tool_summary.needs_write_verification(),
            "tool_feedback": tool_summary
                .tool_feedback
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>(),
        });

        messages.push(ChatMessage {
            role: "assistant".into(),
            content: draft_response.to_string(),
        });
        messages.push(ChatMessage {
            role: "system".into(),
            content: format!(
                concat!(
                    "The previous draft still needs one corrective pass before it can be sent. ",
                    "You may use tools if needed, but make at most one targeted corrective tool pass and do not repeat blocked or identical failing calls. ",
                    "If the answer can be corrected without more tools, do so directly.\n\n",
                    "Task intent: {:?}.\n",
                    "Correction report:\n{}"
                ),
                classification.intent,
                serde_json::to_string_pretty(&remediation_report)
                    .unwrap_or_else(|_| remediation_report.to_string())
            ),
        });
        messages.push(ChatMessage {
            role: "user".into(),
            content: "Take the corrective pass now. Use tools only if they materially improve correctness; otherwise return the corrected final answer directly.".into(),
        });

        messages
    }

    fn default_system_prompt(&self, tool_defs: Option<&[ToolDefinition]>) -> String {
        let mut system = format!(
            concat!(
                "You are NgenOrca, the user's personal AI assistant. ",
                "You are not a raw model passthrough or generic anonymous LLM; ",
                "you are the assistant layer that helps the user across chat, coding, planning, analysis, summarization, translation, workspace tasks, and web tasks.\n\n",
                "Act with clear ownership of the conversation: understand the user's goal, use available capabilities when helpful, and answer directly. ",
                "If a tool would improve accuracy or let you perform the task, use it instead of guessing. ",
                "Do not claim you cannot browse files, run commands, or access the web when those capabilities are available to you through tools. ",
                "If a capability is unavailable, say so plainly.\n\n",
                "Keep responses practical, honest, and grounded in the actual results of your tools and context. ",
                "Reply in the user's language when it is clear from the conversation.\n\n",
                "Primary workspace: {}"
            ),
            self.config.agent.workspace.display()
        );

        if let Some(tool_defs) = tool_defs
            && !tool_defs.is_empty()
        {
            system.push_str("\n\n## Available tools:\n");
            for tool in tool_defs {
                system.push_str(&format!("- {}: {}\n", tool.name, tool.description));
            }
            system.push_str(
                "Use tools when they are relevant, especially for filesystem inspection, searching, reading or writing files, web access, and command execution."
            );
        }

        system
    }

    fn route_from_learned(
        &self,
        classification: &TaskClassification,
        learned_router: &LearnedRouter,
    ) -> Option<RoutingDecision> {
        let diagnostic = learned_router
            .lookup_for_task_with_policy(classification, &self.config.agent.learned_routing)
            .ok()
            .flatten()?;
        let rule = &diagnostic.rule;

        if let Some(max_complexity) = rule.max_complexity
            && classification.complexity > max_complexity
        {
            return None;
        }

        if rule.target_agent == "primary" {
            let mut decision = self.route_to_primary(
                classification,
                &format!(
                    "Learned route: {} (effective {:.2}, raw {:.2}, accept {:.0}%, escalation {:.0}%, failure {:.0}%, stability {:.2}, samples {})",
                    rule.target_agent,
                    diagnostic.effective_confidence,
                    rule.confidence,
                    diagnostic.accept_rate * 100.0,
                    diagnostic.escalation_rate * 100.0,
                    diagnostic.failure_rate * 100.0,
                    diagnostic.stability_score,
                    rule.sample_count,
                ),
            );
            decision.from_memory = true;
            return Some(decision);
        }

        let agent = self.config.sub_agent(&rule.target_agent)?;
        let system_prompt = self.generate_system_prompt(
            agent,
            classification,
            classification.language.as_deref(),
        );

        Some(RoutingDecision {
            target: SubAgentId {
                name: agent.name.clone(),
                model: agent.model.clone(),
            },
            reason: format!(
                "Learned route: {} (effective {:.2}, raw {:.2}, accept {:.0}%, escalation {:.0}%, failure {:.0}%, stability {:.2}, samples {})",
                agent.name,
                diagnostic.effective_confidence,
                rule.confidence,
                diagnostic.accept_rate * 100.0,
                diagnostic.escalation_rate * 100.0,
                diagnostic.failure_rate * 100.0,
                diagnostic.stability_score,
                rule.sample_count,
            ),
            system_prompt,
            temperature: Some(agent.temperature),
            max_tokens: Some(agent.max_tokens),
            from_memory: true,
        })
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
            execution_diagnostics: ExecutionDiagnosticsInfo {
                response_metadata_exposed: true,
                worker_stage_reporting: vec![
                    "parallel-support".into(),
                    "initial".into(),
                    "escalation".into(),
                    "augmentation".into(),
                ],
                tracks_structured_planning: true,
                tracks_tool_verification: true,
                tracks_tool_remediation: true,
                tracks_primary_synthesis: true,
                tracks_branch_contradiction_analysis: true,
                tracks_learned_route_trends: true,
            },
        }
    }

    fn build_delegation_plan(
        &self,
        classification: &TaskClassification,
        routing: &RoutingDecision,
    ) -> Option<DelegationPlan> {
        if !should_build_delegation_plan(classification, routing) {
            return None;
        }

        let primary = SubAgentId {
            name: "primary".into(),
            model: self.config.agent.model.clone(),
        };
        let framing_excludes = [routing.target.name.as_str()];
        let framing_agent = self.find_plan_support_agent(
            classification,
            &["Planning", "Analysis"],
            &framing_excludes,
        );
        let mut review_excludes = vec![routing.target.name.as_str()];
        if let Some(agent) = framing_agent.as_ref() {
            review_excludes.push(agent.name.as_str());
        }
        let review_agent = self.find_plan_support_agent(
            classification,
            &["Analysis", "Reasoning", "QuestionAnswering", "General"],
            &review_excludes,
        );

        let mut steps = Vec::new();
        if let Some(agent) = framing_agent
            && agent.name != "primary"
        {
            steps.push(DelegationPlanStep {
                id: "frame-task".into(),
                goal: "Clarify goals, constraints, and success criteria before deeper execution."
                    .into(),
                agent,
            });
        }

        if let Some(agent) = review_agent
            && agent.name != "primary"
        {
            steps.push(DelegationPlanStep {
                id: "cross-check".into(),
                goal: "Independently review assumptions, risks, and likely gaps while the main worker executes."
                    .into(),
                agent,
            });
        }

        let strategy = match steps.len() {
            0 => "structured-sequential",
            1 => "parallel-framing-and-execution",
            _ => "parallel-multi-branch",
        };

        let execution_goal = match &classification.intent {
            TaskIntent::Coding => {
                "Carry out the repo-specific coding or debugging work with concrete implementation details."
            }
            TaskIntent::Analysis => {
                "Perform the domain analysis and extract the decisive technical findings."
            }
            TaskIntent::Planning => {
                "Turn the request into an actionable execution plan with explicit sequencing and risks."
            }
            TaskIntent::ToolUse => {
                "Coordinate the operational tool work and return grounded results with clear limitations."
            }
            _ => "Complete the main delegated work with task-specific technical substance.",
        };

        steps.push(DelegationPlanStep {
            id: "execute-domain-work".into(),
            goal: execution_goal.into(),
            agent: routing.target.clone(),
        });
        steps.push(DelegationPlanStep {
            id: "final-verify".into(),
            goal: "Check completeness, risks, and user-facing clarity before the final response."
                .into(),
            agent: primary,
        });

        Some(DelegationPlan {
            strategy: strategy.into(),
            steps,
        })
    }

    fn find_plan_support_agent(
        &self,
        classification: &TaskClassification,
        preferred_roles: &[&str],
        exclude_names: &[&str],
    ) -> Option<SubAgentId> {
        self.config
            .agent
            .sub_agents
            .iter()
            .filter(|agent| !exclude_names.iter().any(|name| agent.name == *name))
            .filter(|agent| complexity_within_limit(classification.complexity, &agent.max_complexity))
            .filter(|agent| {
                agent.roles.iter().any(|role| {
                    preferred_roles
                        .iter()
                        .any(|preferred| role.eq_ignore_ascii_case(preferred))
                })
            })
            .min_by_key(|agent| agent.priority)
            .map(|agent| SubAgentId {
                name: agent.name.clone(),
                model: agent.model.clone(),
            })
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
            self.route_to_primary(
                classification,
                &format!("No sub-agent for role: {}", intent_str),
            )
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
                reason: format!(
                    "LocalFirst: {} (local, priority {})",
                    agent.name, agent.priority
                ),
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
    pub execution_diagnostics: ExecutionDiagnosticsInfo,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionDiagnosticsInfo {
    pub response_metadata_exposed: bool,
    pub worker_stage_reporting: Vec<String>,
    pub tracks_structured_planning: bool,
    pub tracks_tool_verification: bool,
    pub tracks_tool_remediation: bool,
    pub tracks_primary_synthesis: bool,
    pub tracks_branch_contradiction_analysis: bool,
    pub tracks_learned_route_trends: bool,
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

fn should_build_delegation_plan(
    classification: &TaskClassification,
    routing: &RoutingDecision,
) -> bool {
    if routing.target.name == "primary" {
        return false;
    }

    classification.complexity >= TaskComplexity::Complex
        || (classification.complexity >= TaskComplexity::Moderate
            && matches!(
                classification.intent,
                TaskIntent::Planning | TaskIntent::Analysis | TaskIntent::Coding | TaskIntent::ToolUse
            ))
}

fn render_delegation_plan(plan: &DelegationPlan) -> String {
    let mut rendered = format!("Strategy: {}\n", plan.strategy);
    for step in &plan.steps {
        rendered.push_str(&format!(
            "- {} via {}/{}: {}\n",
            step.id, step.agent.name, step.agent.model, step.goal
        ));
    }
    rendered.push_str(
        "Follow the plan unless the evidence in the current request clearly requires a safer adjustment.",
    );
    rendered
}

fn delegation_plan_diagnostics(plan: &DelegationPlan) -> DelegationPlanDiagnostics {
    DelegationPlanDiagnostics {
        strategy: plan.strategy.clone(),
        steps: plan
            .steps
            .iter()
            .map(|step| DelegationStepDiagnostics {
                id: step.id.clone(),
                goal: step.goal.clone(),
                agent: step.agent.clone(),
            })
            .collect(),
    }
}

fn parallel_support_steps(plan: &DelegationPlan, main_target: &SubAgentId) -> Vec<DelegationPlanStep> {
    if !plan.strategy.starts_with("parallel") {
        return Vec::new();
    }

    plan.steps
        .iter()
        .filter(|step| step.agent.name != main_target.name && step.agent.name != "primary")
        .cloned()
        .collect()
}

fn merge_usage(total: &mut Usage, usage: &Usage) {
    total.prompt_tokens += usage.prompt_tokens;
    total.completion_tokens += usage.completion_tokens;
    total.total_tokens += usage.total_tokens;
}

fn should_primary_synthesize(served_by: &SubAgentId) -> bool {
    served_by.name != "primary"
}

#[derive(Debug, Clone)]
struct DelegationPlan {
    strategy: String,
    steps: Vec<DelegationPlanStep>,
}

#[derive(Debug, Clone)]
struct DelegationPlanStep {
    id: String,
    goal: String,
    agent: SubAgentId,
}

#[derive(Debug, Clone)]
struct BranchMemoryView {
    context: ngenorca_memory::ContextPack,
    memory_scope: String,
    evidence_focus: String,
    evidence_items: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct BranchMemoryPolicy {
    memory_scope: &'static str,
    evidence_focus: &'static str,
    semantic_limit: usize,
    episodic_limit: usize,
    working_limit: usize,
}

#[derive(Debug, Clone)]
struct SpecialistDraft {
    stage: String,
    agent: SubAgentId,
    content: String,
    note: Option<String>,
    branch_role: String,
    reliability: String,
    priority_weight: u8,
    memory_scope: Option<String>,
    evidence_focus: Option<String>,
    evidence_items: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ToolVerificationReport {
    grounded: bool,
    should_retry_tools: bool,
    corrected_answer: String,
    retry_instruction: Option<String>,
    #[serde(default)]
    issues: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ToolLoopSummary {
    rounds: usize,
    tool_names: Vec<String>,
    tool_feedback: Vec<ChatMessage>,
    tool_observations: Vec<ToolObservation>,
    had_failures: bool,
    had_blocked_calls: bool,
    retry_guidance: Vec<String>,
    attempt_trace: Vec<CorrectionAttemptTrace>,
}

#[derive(Debug, Clone)]
struct ToolObservation {
    tool: String,
    ok: bool,
    retryable: bool,
    result: Option<serde_json::Value>,
    error: Option<String>,
}

fn execution_plan_step<'a>(
    plan: &'a DelegationPlan,
    main_target: &SubAgentId,
) -> Option<&'a DelegationPlanStep> {
    plan.steps
        .iter()
        .find(|step| step.id == "execute-domain-work" || step.agent.name == main_target.name)
}

fn branch_memory_policy(step_id: &str) -> BranchMemoryPolicy {
    match step_id {
        "frame-task" => BranchMemoryPolicy {
            memory_scope: "goal-and-constraint slice",
            evidence_focus: "Prioritize goals, constraints, user preferences, and the most recent active context that should shape planning.",
            semantic_limit: 3,
            episodic_limit: 2,
            working_limit: 3,
        },
        "cross-check" => BranchMemoryPolicy {
            memory_scope: "risk-and-gap slice",
            evidence_focus: "Prioritize assumptions, prior failure signals, and corroborating snippets useful for an independent cross-check.",
            semantic_limit: 2,
            episodic_limit: 3,
            working_limit: 2,
        },
        _ => BranchMemoryPolicy {
            memory_scope: "execution slice",
            evidence_focus: "Prioritize technically actionable preferences, relevant prior attempts, and the latest working context needed for concrete execution.",
            semantic_limit: 5,
            episodic_limit: 4,
            working_limit: 4,
        },
    }
}

fn semantic_category_weight(
    step_id: &str,
    category: &ngenorca_memory::semantic::FactCategory,
) -> u8 {
    use ngenorca_memory::semantic::FactCategory;

    match step_id {
        "frame-task" => match category {
            FactCategory::Goal => 0,
            FactCategory::Preference | FactCategory::TechnicalPreference => 1,
            FactCategory::Knowledge | FactCategory::Other(_) => 2,
            _ => 3,
        },
        "cross-check" => match category {
            FactCategory::Knowledge | FactCategory::Other(_) => 0,
            FactCategory::Goal | FactCategory::TechnicalPreference => 1,
            FactCategory::Preference => 2,
            _ => 3,
        },
        _ => match category {
            FactCategory::TechnicalPreference => 0,
            FactCategory::Goal | FactCategory::Knowledge => 1,
            FactCategory::Preference => 2,
            _ => 3,
        },
    }
}

fn build_branch_memory_view(
    step: &DelegationPlanStep,
    memory_context: Option<&ngenorca_memory::ContextPack>,
) -> Option<BranchMemoryView> {
    let ctx = memory_context?;
    let policy = branch_memory_policy(&step.id);

    let mut semantic_block = ctx.semantic_block.clone();
    semantic_block.sort_by(|left, right| {
        semantic_category_weight(&step.id, &left.category)
            .cmp(&semantic_category_weight(&step.id, &right.category))
            .then_with(|| right.confidence.total_cmp(&left.confidence))
            .then_with(|| right.access_count.cmp(&left.access_count))
    });
    semantic_block.truncate(policy.semantic_limit.min(semantic_block.len()));

    let episodic_snippets = ctx
        .episodic_snippets
        .iter()
        .take(policy.episodic_limit)
        .cloned()
        .collect::<Vec<_>>();

    let mut working_messages = ctx
        .working_messages
        .iter()
        .rev()
        .filter(|message| !message.content.trim().is_empty())
        .take(policy.working_limit)
        .cloned()
        .collect::<Vec<_>>();
    working_messages.reverse();

    let total_estimated_tokens = semantic_block
        .iter()
        .map(|fact| fact.fact.len())
        .sum::<usize>()
        / 4
        + episodic_snippets.iter().map(|entry| entry.content.len()).sum::<usize>() / 4
        + working_messages
            .iter()
            .map(|message| message.content.len())
            .sum::<usize>()
            / 4;

    let context = ngenorca_memory::ContextPack {
        semantic_block,
        episodic_snippets,
        working_messages,
        total_estimated_tokens,
    };

    let mut evidence_items = Vec::new();
    for fact in &context.semantic_block {
        evidence_items.push(format!(
            "semantic::{}::{} (confidence: {:.0}%)",
            semantic_category_label(&fact.category),
            fact.fact,
            fact.confidence * 100.0,
        ));
    }
    for episode in &context.episodic_snippets {
        let summary = if episode.summary.as_deref().unwrap_or_default().trim().is_empty() {
            trim_snippet(&episode.content, 120)
        } else {
            trim_snippet(episode.summary.as_deref().unwrap_or_default(), 120)
        };
        evidence_items.push(format!(
            "episodic::{}::{}",
            episode.timestamp.format("%Y-%m-%d"),
            summary,
        ));
    }
    for message in &context.working_messages {
        evidence_items.push(format!(
            "working::{}::{}",
            message.role,
            trim_snippet(&message.content, 100),
        ));
    }

    Some(BranchMemoryView {
        context,
        memory_scope: policy.memory_scope.into(),
        evidence_focus: policy.evidence_focus.into(),
        evidence_items,
    })
}

fn semantic_category_label(category: &ngenorca_memory::semantic::FactCategory) -> String {
    use ngenorca_memory::semantic::FactCategory;

    match category {
        FactCategory::Preference => "preference".into(),
        FactCategory::PersonalInfo => "personal_info".into(),
        FactCategory::Relationship => "relationship".into(),
        FactCategory::Routine => "routine".into(),
        FactCategory::TechnicalPreference => "technical_preference".into(),
        FactCategory::ImportantDate => "important_date".into(),
        FactCategory::Goal => "goal".into(),
        FactCategory::Knowledge => "knowledge".into(),
        FactCategory::Other(value) => format!("other:{}", value),
    }
}

fn trim_snippet(value: &str, limit: usize) -> String {
    if value.len() > limit {
        format!("{}…", &value[..limit])
    } else {
        value.to_string()
    }
}

fn branch_evidence_diagnostics(
    stage: &str,
    agent: &SubAgentId,
    branch_role: &str,
    view: &BranchMemoryView,
) -> BranchEvidenceDiagnostics {
    BranchEvidenceDiagnostics {
        stage: stage.into(),
        agent: agent.clone(),
        branch_role: branch_role.into(),
        memory_scope: view.memory_scope.clone(),
        evidence_focus: view.evidence_focus.clone(),
        evidence_items: view.evidence_items.clone(),
    }
}

impl ToolLoopSummary {
    fn used_tools(&self) -> bool {
        !self.tool_names.is_empty()
    }

    fn requires_follow_up_verification(&self) -> bool {
        self.had_failures
            || self.needs_write_verification()
            || self.needs_command_verification()
    }

    fn merge(&mut self, mut other: Self) {
        self.rounds += other.rounds;
        self.tool_names.append(&mut other.tool_names);
        self.tool_feedback.append(&mut other.tool_feedback);
        self.tool_observations.append(&mut other.tool_observations);
        self.had_failures |= other.had_failures;
        self.had_blocked_calls |= other.had_blocked_calls;
        for guidance in other.retry_guidance.drain(..) {
            push_unique(&mut self.retry_guidance, guidance);
        }
        self.attempt_trace.append(&mut other.attempt_trace);
    }

    fn remember_retry_guidance(
        &mut self,
        tool_name: &str,
        outcome: &ToolExecutionOutcome,
    ) -> Option<String> {
        let guidance = tool_retry_guidance(tool_name, outcome);
        if let Some(guidance) = guidance.as_ref() {
            push_unique(&mut self.retry_guidance, guidance.clone());
        }
        guidance
    }

    fn retry_instruction(&self) -> Option<String> {
        let mut guidance = self.retry_guidance.clone();
        if let Some(recovery_guidance) = self.repeated_failure_guidance() {
            push_unique(&mut guidance, recovery_guidance);
        }

        if guidance.is_empty() {
            return None;
        }

        Some(format!(
            "Targeted retry guidance:\n- {}",
            guidance.join("\n- ")
        ))
    }

    fn needs_write_verification(&self) -> bool {
        let last_write = self
            .tool_names
            .iter()
            .rposition(|name| name == "write_file");
        let last_readback = self
            .tool_names
            .iter()
            .rposition(|name| name == "read_file" || name == "grep_workspace");

        match last_write {
            Some(last_write) => last_readback.map_or(true, |last_readback| last_readback < last_write),
            None => false,
        }
    }

    fn needs_command_verification(&self) -> bool {
        if !self
            .tool_observations
            .iter()
            .any(|observation| observation.tool == "run_command")
        {
            return false;
        }

        let last_write = self
            .tool_observations
            .iter()
            .rposition(|observation| observation.tool == "write_file" && observation.ok);
        let last_command_success = self.tool_observations.iter().rposition(|observation| {
            observation.tool == "run_command" && observation.command_succeeded()
        });
        let last_command_failure = self.tool_observations.iter().rposition(|observation| {
            observation.tool == "run_command" && !observation.command_succeeded()
        });

        if let Some(last_failure) = last_command_failure
            && match last_command_success {
                Some(last_success) => last_success < last_failure,
                None => true,
            }
        {
            return true;
        }

        if let Some(last_write) = last_write
            && match last_command_success {
                Some(last_success) => last_success < last_write,
                None => true,
            }
        {
            return true;
        }

        false
    }

    fn repeated_failure_guidance(&self) -> Option<String> {
        let repeated_tools = self.repeated_failure_tools();
        if repeated_tools.is_empty() {
            return None;
        }

        Some(format!(
            "After repeated failures with {}, stop retrying the same recovery path. Switch tools, narrow the verification step, or answer with a concrete limitation grounded in the observed results.",
            repeated_tools.join(", ")
        ))
    }

    fn repeated_failure_tools(&self) -> Vec<String> {
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for observation in &self.tool_observations {
            if observation.effective_failure() {
                *counts.entry(observation.tool.clone()).or_insert(0) += 1;
            }
        }

        counts
            .into_iter()
            .filter_map(|(tool, count)| (count >= 2).then_some(tool))
            .collect()
    }

    fn should_abandon_tool_retries(&self) -> bool {
        !self.repeated_failure_tools().is_empty() && (self.had_blocked_calls || self.attempt_trace.len() >= 4)
    }

    fn latest_command_failure_issue(&self) -> Option<String> {
        self.tool_observations
            .iter()
            .rev()
            .find(|observation| observation.tool == "run_command" && !observation.command_succeeded())
            .map(|observation| observation.command_failure_issue())
    }

    fn default_verification_issues(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.had_failures {
            issues.push("one or more tool invocations failed".into());
        }
        if self.had_blocked_calls {
            issues.push("one or more repeated tool calls were blocked to prevent shallow retry loops".into());
        }
        if self.needs_write_verification() {
            issues.push("file edits were not read back after the last write".into());
        }
        if self.needs_command_verification() {
            issues.push(
                "edited files were not revalidated with a successful verification command after the last relevant change".into(),
            );
        }
        if let Some(command_issue) = self.latest_command_failure_issue() {
            push_unique(&mut issues, command_issue);
        }

        issues
    }

    fn follow_up_verification_instruction(&self) -> Option<String> {
        let mut instructions: Vec<String> = Vec::new();

        if self.needs_write_verification() {
            instructions.push(
                "You used `write_file`. Before giving the final answer, verify the written content with `read_file` or `grep_workspace` so your answer reflects the actual file state.".into(),
            );
        }

        if self.needs_command_verification() {
            instructions.push(
                "You appear to be in an edit/build or edit/test workflow. After the latest relevant file change, run one focused verification command and base your final answer on that latest command result instead of an earlier build/test run.".into(),
            );
        }

        if instructions.is_empty() {
            None
        } else {
            Some(instructions.join(" "))
        }
    }

    fn verification_hints(&self) -> Vec<String> {
        let mut hints = Vec::new();

        if self.tool_names.iter().any(|name| name == "read_file") {
            hints.push(
                "Only claim file contents that appear in the returned `read_file` line range.".into(),
            );
        }
        if self.tool_names.iter().any(|name| name == "write_file") {
            hints.push(
                "Only claim a file edit succeeded if `write_file` returned success and the resulting content was read back or otherwise confirmed.".into(),
            );
        }
        if self.tool_names.iter().any(|name| name == "run_command") {
            hints.push(
                "Only claim a command, build, or test succeeded when the tool result shows `exit_code` 0 and the stdout/stderr support that claim.".into(),
            );
            if self.needs_command_verification() {
                hints.push(
                    "If you edited files, only claim the fix is complete after a successful verification command that ran after the latest relevant edit.".into(),
                );
            }
        }
        if self
            .tool_names
            .iter()
            .any(|name| name == "fetch_url" || name == "web_search")
        {
            hints.push(
                "Only claim web facts that appear in the fetched page or search results, and keep uncertainty if the source material is partial.".into(),
            );
        }

        hints
    }
}

impl ToolObservation {
    fn from_outcome(tool_name: &str, outcome: &ToolExecutionOutcome) -> Self {
        match outcome {
            ToolExecutionOutcome::Success { value } => Self {
                tool: tool_name.into(),
                ok: true,
                retryable: false,
                result: Some(value.clone()),
                error: None,
            },
            ToolExecutionOutcome::Failed { error } => Self {
                tool: tool_name.into(),
                ok: false,
                retryable: true,
                result: None,
                error: Some(error.clone()),
            },
            ToolExecutionOutcome::Blocked { error } => Self {
                tool: tool_name.into(),
                ok: false,
                retryable: false,
                result: None,
                error: Some(error.clone()),
            },
        }
    }

    fn effective_failure(&self) -> bool {
        if self.tool == "run_command" {
            return !self.command_succeeded();
        }

        !self.ok
    }

    fn command_succeeded(&self) -> bool {
        if self.tool != "run_command" {
            return self.ok;
        }

        if !self.ok {
            return false;
        }

        let Some(result) = self.result.as_ref() else {
            return false;
        };

        if result.get("timed_out").and_then(|value| value.as_bool()) == Some(true) {
            return false;
        }

        result
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or_else(|| result.get("exit_code").and_then(|value| value.as_i64()) == Some(0))
    }

    fn command_failure_issue(&self) -> String {
        let command = self
            .result
            .as_ref()
            .and_then(|result| result.get("command"))
            .and_then(|value| value.as_str())
            .unwrap_or("run_command");

        if let Some(result) = self.result.as_ref() {
            if result.get("timed_out").and_then(|value| value.as_bool()) == Some(true) {
                return format!("verification command '{command}' timed out");
            }

            if let Some(exit_code) = result.get("exit_code").and_then(|value| value.as_i64()) {
                return format!("verification command '{command}' exited with code {exit_code}");
            }
        }

        if self.retryable {
            if let Some(error) = self.error.as_deref() {
                return format!("verification command '{command}' failed: {error}");
            }
        }

        format!("verification command '{command}' did not complete successfully")
    }
}

#[derive(Debug, Clone)]
enum ToolExecutionOutcome {
    Success { value: serde_json::Value },
    Failed { error: String },
    Blocked { error: String },
}

impl ToolExecutionOutcome {
    fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Success { .. } => "success",
            Self::Failed { .. } => "failed",
            Self::Blocked { .. } => "blocked",
        }
    }

    fn failure_class(&self, tool_name: &str) -> Option<String> {
        let message = match self {
            Self::Success { .. } => return None,
            Self::Failed { error } | Self::Blocked { error } => error.to_ascii_lowercase(),
        };

        let class = if matches!(self, Self::Blocked { .. }) {
            "blocked-duplicate"
        } else if message.contains("not found") || message.contains("no such file") {
            "path"
        } else if message.contains("permission") || message.contains("access is denied") {
            "permission"
        } else if message.contains("timeout") || message.contains("timed out") {
            "timeout"
        } else if message.contains("sandbox") || message.contains("restricted") {
            "sandbox"
        } else if tool_name == "run_command" {
            "execution"
        } else {
            "arguments"
        };

        Some(class.into())
    }
}

#[derive(Debug, Default, Clone)]
struct ContradictionScan {
    score: f64,
    conflicting_branches: usize,
    anchor_stage: Option<String>,
    summary: Vec<String>,
    signals: Vec<String>,
}

fn contradiction_scan(drafts: &[SpecialistDraft]) -> ContradictionScan {
    let Some(anchor) = drafts
        .iter()
        .filter(|draft| draft.branch_role == "execution")
        .max_by_key(|draft| draft.priority_weight)
        .or_else(|| drafts.iter().max_by_key(|draft| draft.priority_weight))
    else {
        return ContradictionScan::default();
    };

    let anchor_tokens = normalized_tokens(&anchor.content);
    if anchor_tokens.is_empty() {
        return ContradictionScan::default();
    }

    let mut summary = Vec::new();
    let mut signals = Vec::new();
    let mut max_score = 0.0_f64;
    let anchor_actions = action_polarity_map(&anchor.content);
    let anchor_numbers = numeric_fragments(&anchor.content);

    for draft in drafts.iter().filter(|draft| draft.stage != anchor.stage) {
        let branch_tokens = normalized_tokens(&draft.content);
        if branch_tokens.is_empty() {
            continue;
        }

        let overlap = token_overlap_ratio(&anchor_tokens, &branch_tokens);
        let branch_actions = action_polarity_map(&draft.content);
        let branch_numbers = numeric_fragments(&draft.content);
        let conflicting_actions = contradictory_actions(&anchor_actions, &branch_actions);
        let has_numeric_conflict = numeric_conflict(&anchor_numbers, &branch_numbers, overlap);
        let has_conflict_markers = contains_conflict_markers(&draft.content);
        let overlap_divergence = (1.0 - overlap) * 0.18;
        let marker_bias = has_conflict_markers as u8 as f64 * 0.16;
        let action_bias = (conflicting_actions.len().min(3) as f64) * 0.18;
        let numeric_bias = has_numeric_conflict as u8 as f64 * 0.22;
        let priority_bias = if draft.priority_weight >= anchor.priority_weight {
            0.08
        } else if draft.branch_role == anchor.branch_role {
            0.04
        } else {
            0.0
        };
        let reliability_bias = if anchor.reliability == "grounded" && draft.reliability != "grounded"
        {
            0.04
        } else {
            0.0
        };
        let score = (
            overlap_divergence + marker_bias + action_bias + numeric_bias + priority_bias + reliability_bias
        )
        .clamp(0.0, 1.0);
        let materially_conflicting = !conflicting_actions.is_empty()
            || has_numeric_conflict
            || (has_conflict_markers && overlap < 0.62);

        if materially_conflicting && score >= 0.38 {
            max_score = max_score.max(score);
            let mut reasons = Vec::new();
            if !conflicting_actions.is_empty() {
                let mut action_list = conflicting_actions.iter().cloned().collect::<Vec<_>>();
                action_list.sort();
                let signal = format!("negated_action_overlap: {}", action_list.join(", "));
                push_unique(&mut reasons, signal.clone());
                push_unique(&mut signals, signal);
            }
            if has_numeric_conflict {
                let signal = "numeric_mismatch".to_string();
                push_unique(&mut reasons, signal.clone());
                push_unique(&mut signals, signal);
            }
            if has_conflict_markers {
                let signal = "conflict_markers_present".to_string();
                push_unique(&mut reasons, signal.clone());
                push_unique(&mut signals, signal);
            }
            if overlap < 0.45 {
                let signal = format!("low_semantic_overlap:{:.2}", overlap);
                push_unique(&mut reasons, signal.clone());
                push_unique(&mut signals, signal);
            }
            summary.push(format!(
                "{} via {}/{} diverged from the {} anchor (score {:.2}; reasons: {})",
                draft.stage,
                draft.agent.name,
                draft.agent.model,
                anchor.stage,
                score,
                reasons.join(", ")
            ));
        }
    }

    ContradictionScan {
        score: max_score,
        conflicting_branches: summary.len(),
        anchor_stage: Some(anchor.stage.clone()),
        summary,
        signals,
    }
}

fn normalized_tokens(content: &str) -> std::collections::BTreeSet<String> {
    content
        .split(|ch: char| !ch.is_alphanumeric())
        .filter_map(|token| {
            let normalized = token.trim().to_ascii_lowercase();
            if normalized.len() >= 4 {
                Some(normalized)
            } else {
                None
            }
        })
        .collect()
}

fn token_overlap_ratio(
    left: &std::collections::BTreeSet<String>,
    right: &std::collections::BTreeSet<String>,
) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        return 1.0;
    }

    let intersection = left.intersection(right).count();
    intersection as f64 / union as f64
}

fn contains_conflict_markers(content: &str) -> bool {
    let lowered = content.to_ascii_lowercase();
    [
        "however",
        "instead",
        "not",
        "contradict",
        "conflict",
        "mismatch",
        "but",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

fn action_polarity_map(content: &str) -> std::collections::BTreeMap<String, bool> {
    let mut actions = std::collections::BTreeMap::new();
    let negators = ["not", "never", "avoid", "skip", "without", "instead", "don't"];

    for raw_sentence in content
        .split(['.', '!', '?', '\n', ';'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
    {
        let lowered = raw_sentence.to_ascii_lowercase();
        let tokens = lowered
            .split(|ch: char| !ch.is_alphanumeric() && ch != '-')
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();

        for (index, token) in tokens.iter().enumerate() {
            let normalized = normalize_action_token(token);
            if normalized.is_empty() {
                continue;
            }

            let negated = tokens[index.saturating_sub(3)..index]
                .iter()
                .any(|prior| negators.contains(prior));
            actions.entry(normalized).or_insert(!negated);
        }
    }

    actions
}

fn normalize_action_token(token: &str) -> String {
    let candidate = token.trim_matches('-');
    if candidate.len() < 4 || common_stopword(candidate) {
        return String::new();
    }

    let mut normalized = candidate.to_string();
    for suffix in ["ing", "ed", "es", "s"] {
        if normalized.len() > suffix.len() + 2 && normalized.ends_with(suffix) {
            normalized.truncate(normalized.len() - suffix.len());
            break;
        }
    }

    normalized
}

fn common_stopword(token: &str) -> bool {
    matches!(
        token,
        "this"
            | "that"
            | "with"
            | "from"
            | "into"
            | "then"
            | "until"
            | "while"
            | "should"
            | "would"
            | "could"
            | "must"
            | "need"
            | "keep"
            | "make"
            | "have"
            | "your"
            | "them"
            | "there"
            | "their"
            | "avoid"
    )
}

fn contradictory_actions(
    anchor: &std::collections::BTreeMap<String, bool>,
    branch: &std::collections::BTreeMap<String, bool>,
) -> std::collections::BTreeSet<String> {
    anchor
        .iter()
        .filter_map(|(action, anchor_positive)| {
            branch.get(action).and_then(|branch_positive| {
                if anchor_positive != branch_positive {
                    Some(action.clone())
                } else {
                    None
                }
            })
        })
        .collect()
}

fn numeric_fragments(content: &str) -> std::collections::BTreeSet<String> {
    content
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == ':'))
        .filter_map(|fragment| {
            let trimmed = fragment.trim().to_ascii_lowercase();
            if trimmed.chars().any(|ch| ch.is_ascii_digit()) {
                Some(trimmed)
            } else {
                None
            }
        })
        .collect()
}

fn numeric_conflict(
    anchor_numbers: &std::collections::BTreeSet<String>,
    branch_numbers: &std::collections::BTreeSet<String>,
    overlap: f64,
) -> bool {
    !anchor_numbers.is_empty()
        && !branch_numbers.is_empty()
        && anchor_numbers.is_disjoint(branch_numbers)
        && overlap >= 0.18
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn tool_retry_guidance(tool_name: &str, outcome: &ToolExecutionOutcome) -> Option<String> {
    match outcome {
        ToolExecutionOutcome::Success { .. } => None,
        ToolExecutionOutcome::Blocked { .. } => Some(format!(
            "Do not repeat `{tool_name}` with identical arguments. Change the input, use a different tool, or explain the limitation."
        )),
        ToolExecutionOutcome::Failed { .. } => match tool_name {
            "read_file" => Some(
                "For `read_file` failures, verify the relative workspace path first, or use `list_dir`/`grep_workspace` to locate the file before retrying once.".into(),
            ),
            "write_file" => Some(
                "For `write_file` failures, verify the target path and parent directory, then retry once. After a successful write, read the file back before claiming the change is complete.".into(),
            ),
            "run_command" => Some(
                "For `run_command` failures, inspect stdout/stderr, verify the working directory, and use an explicit shell executable for shell built-ins before retrying once.".into(),
            ),
            "fetch_url" => Some(
                "For `fetch_url` failures, verify the URL scheme and host. If direct fetch is unreliable, refine the target with `web_search` before retrying once.".into(),
            ),
            "web_search" => Some(
                "For `web_search` failures, tighten the query terms and retry once. Fetch a concrete result before making specific factual claims.".into(),
            ),
            _ => Some(format!(
                "For `{tool_name}` failures, fix the arguments before retrying once, or continue with a clearly stated limitation."
            )),
        },
    }
}

fn verification_diagnostics(report: &ToolVerificationReport) -> VerificationDiagnostics {
    VerificationDiagnostics {
        grounded: report.grounded,
        should_retry_tools: report.should_retry_tools,
        issues: report.issues.clone(),
        retry_instruction: report.retry_instruction.clone(),
    }
}

fn worker_contract(agent: &SubAgentConfig, classification: &TaskClassification) -> String {
    let mut contract = String::from(
        "You are a delegated specialist working on behalf of NgenOrca, the primary assistant. \
Do not present yourself as a separate assistant, do not mention internal routing, and do not ask the user to choose between agents. \
Return content that NgenOrca can send directly to the user."
    );

    contract.push_str(&format!(
        "\nDelegation brief: role={}, complexity={:?}.",
        agent.name, classification.complexity
    ));

    if !classification.domain_tags.is_empty() {
        contract.push_str(&format!(
            " Focus domains: {}.",
            classification.domain_tags.join(", ")
        ));
    }

    contract.push_str(
        " Keep the output practical and concise; include assumptions or risks only when they materially affect correctness.",
    );

    contract
}

fn remember_specialist_draft(
    drafts: &mut Vec<SpecialistDraft>,
    stage: &str,
    agent: &SubAgentId,
    content: Option<&str>,
    note: Option<String>,
    memory_view: Option<&BranchMemoryView>,
) {
    let Some(content) = content.map(str::trim).filter(|content| !content.is_empty()) else {
        return;
    };
    let (branch_role, reliability, priority_weight) = draft_policy(stage);

    drafts.push(SpecialistDraft {
        stage: stage.into(),
        agent: agent.clone(),
        content: content.into(),
        note,
        branch_role: branch_role.into(),
        reliability: reliability.into(),
        priority_weight,
        memory_scope: memory_view.map(|view| view.memory_scope.clone()),
        evidence_focus: memory_view.map(|view| view.evidence_focus.clone()),
        evidence_items: memory_view
            .map(|view| view.evidence_items.clone())
            .unwrap_or_default(),
    });
}

fn specialist_draft_history(drafts: &[SpecialistDraft], latest_worker_response: &str) -> String {
    let mut sections = Vec::new();

    for draft in drafts {
        let mut section = format!(
            "Stage: {}\nAgent: {}/{}\nRole: {}\nReliability: {}\nPriority: {}",
            draft.stage,
            draft.agent.name,
            draft.agent.model,
            draft.branch_role,
            draft.reliability,
            draft.priority_weight,
        );
        if let Some(note) = &draft.note {
            section.push_str(&format!("\nNote: {}", note));
        }
        if let Some(memory_scope) = &draft.memory_scope {
            section.push_str(&format!("\nMemory scope: {}", memory_scope));
        }
        if let Some(evidence_focus) = &draft.evidence_focus {
            section.push_str(&format!("\nEvidence focus: {}", evidence_focus));
        }
        if !draft.evidence_items.is_empty() {
            section.push_str("\nEvidence slice:");
            for item in &draft.evidence_items {
                section.push_str(&format!("\n- {}", item));
            }
        }
        section.push_str(&format!("\nDraft:\n{}", draft.content));
        sections.push(section);
    }

    let latest = latest_worker_response.trim();
    if !latest.is_empty()
        && drafts
            .last()
            .map(|draft| draft.content.trim() != latest)
            .unwrap_or(true)
    {
        sections.push(format!(
            "Stage: latest_worker_response\nAgent: current-worker\nDraft:\n{}",
            latest
        ));
    }

    sections.join("\n\n---\n\n")
}

fn specialist_branch_policy_summary(drafts: &[SpecialistDraft]) -> String {
    if drafts.is_empty() {
        return String::new();
    }

    let mut ordered = drafts.to_vec();
    ordered.sort_by(|left, right| {
        right
            .priority_weight
            .cmp(&left.priority_weight)
            .then_with(|| left.stage.cmp(&right.stage))
    });

    let mut lines = vec![
        "- Use the highest-priority grounded or corrected execution draft as the main factual anchor.".into(),
        "- Use advisory support branches mainly for risks, caveats, and cross-checks unless they directly confirm the anchor draft.".into(),
        "- If a lower-priority branch conflicts with a higher-priority grounded draft, keep the grounded draft and mention the uncertainty only if it materially affects the answer.".into(),
        "- Weighted drafts observed in this request:".into(),
    ];

    for draft in ordered {
        lines.push(format!(
            "  - {} via {}/{} => role={}, reliability={}, priority={}",
            draft.stage,
            draft.agent.name,
            draft.agent.model,
            draft.branch_role,
            draft.reliability,
            draft.priority_weight
        ));
    }

    lines.join("\n")
}

fn draft_policy(stage: &str) -> (&'static str, &'static str, u8) {
    match stage {
        "verified" => ("execution", "grounded", 100),
        "escalation" => ("execution", "corrected", 90),
        "augmentation" => ("execution", "corrected", 85),
        "initial" => ("execution", "working", 70),
        "parallel-support" => ("support", "advisory", 40),
        _ => ("support", "working", 50),
    }
}

fn parse_tool_verification_report(
    raw: Option<&str>,
    fallback_answer: &str,
    tool_summary: &ToolLoopSummary,
) -> ToolVerificationReport {
    let should_retry_by_default = tool_summary.requires_follow_up_verification();
    let default_issues = tool_summary.default_verification_issues();
    let fallback = || ToolVerificationReport {
        grounded: !should_retry_by_default,
        should_retry_tools: should_retry_by_default,
        corrected_answer: fallback_answer.to_string(),
        retry_instruction: tool_summary.retry_instruction(),
        issues: default_issues.clone(),
    };

    let Some(raw) = raw.map(strip_json_fence).filter(|raw| !raw.trim().is_empty()) else {
        return fallback();
    };

    match serde_json::from_str::<ToolVerificationReport>(&raw) {
        Ok(mut report) => {
            if report.corrected_answer.trim().is_empty() {
                report.corrected_answer = fallback_answer.to_string();
            }
            if report.should_retry_tools && report.retry_instruction.is_none() {
                report.retry_instruction = tool_summary.retry_instruction();
            }
            if report.issues.is_empty() && (!report.grounded || report.should_retry_tools) {
                report.issues = default_issues.clone();
            }
            report
        }
        Err(_) => ToolVerificationReport {
            grounded: !should_retry_by_default,
            should_retry_tools: should_retry_by_default,
            corrected_answer: raw.trim().to_string(),
            retry_instruction: tool_summary.retry_instruction(),
            issues: default_issues,
        },
    }
}

fn strip_json_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed.strip_prefix("```json") {
        return stripped.trim().trim_end_matches("```").trim().to_string();
    }
    if let Some(stripped) = trimmed.strip_prefix("```") {
        return stripped.trim().trim_end_matches("```").trim().to_string();
    }
    trimmed.to_string()
}

fn tool_verified_response(
    draft_response: &ChatCompletionResponse,
    report: &ToolVerificationReport,
) -> ChatCompletionResponse {
    ChatCompletionResponse {
        content: Some(report.corrected_answer.clone()),
        tool_calls: vec![],
        usage: draft_response.usage.clone(),
    }
}

fn tool_call_signature(tc: &ToolCallResponse) -> String {
    format!(
        "{}:{}",
        tc.name,
        serde_json::to_string(&tc.arguments).unwrap_or_default()
    )
}

fn format_tool_feedback(tc: &ToolCallResponse, outcome: &ToolExecutionOutcome) -> String {
    let payload = match outcome {
        ToolExecutionOutcome::Success { value } => serde_json::json!({
            "tool": tc.name,
            "call_id": tc.id,
            "ok": true,
            "result": value,
        }),
        ToolExecutionOutcome::Failed { error } => serde_json::json!({
            "tool": tc.name,
            "call_id": tc.id,
            "ok": false,
            "retryable": true,
            "error": error,
        }),
        ToolExecutionOutcome::Blocked { error } => serde_json::json!({
            "tool": tc.name,
            "call_id": tc.id,
            "ok": false,
            "retryable": false,
            "error": error,
        }),
    };

    format!(
        "[Tool: {} (call_id: {})]\n{}",
        tc.name,
        tc.id,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngenorca_config::SubAgentConfig;
    use ngenorca_core::orchestration::{ClassificationMethod, CorrectionRecord, OrchestrationRecord, QualityMethod, QualityVerdict, SynthesisRecord};
    use crate::orchestration::LearnedRouter;

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

    fn learned_record(classification: TaskClassification, target: &str) -> OrchestrationRecord {
        OrchestrationRecord {
            classification,
            routing: RoutingDecision {
                target: SubAgentId {
                    name: target.into(),
                    model: "test-model".into(),
                },
                reason: "test learned route".into(),
                system_prompt: String::new(),
                temperature: None,
                max_tokens: None,
                from_memory: false,
            },
            quality: QualityVerdict::Accept { score: Some(0.9) },
            quality_method: QualityMethod::Heuristic,
            escalated: false,
            user_id: Some(UserId("test-user".into())),
            channel: Some("web".into()),
            latency_ms: 10,
            total_tokens: 20,
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
                attempted: false,
                succeeded: false,
                contradiction_score: 0.0,
                conflicting_branches: 0,
            },
            timestamp: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_classify_summarization() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);

        let result = orch
            .classify("resume este artigo sobre redes", None)
            .await
            .unwrap();
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
    fn test_route_with_learned_primary_without_sub_agents() {
        let config = Arc::new(NgenOrcaConfig::default());
        let orch = HybridOrchestrator::new(config);
        let learned_router = LearnedRouter::new(":memory:").unwrap();
        let classification = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: TaskComplexity::Simple,
            confidence: 0.95,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec![],
            language: Some("en".into()),
        };
        learned_router
            .ingest(&learned_record(classification.clone(), "primary"))
            .unwrap();

        let decision = orch.route_with_learned(&classification, Some(&learned_router));
        assert_eq!(decision.target.name, "primary");
        assert!(decision.from_memory);
        assert!(decision.reason.contains("Learned route"));
        assert!(decision.reason.contains("accept"));
        assert!(decision.reason.contains("stability"));
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
        assert!(info.execution_diagnostics.response_metadata_exposed);
        assert!(info
            .execution_diagnostics
            .worker_stage_reporting
            .contains(&"parallel-support".to_string()));
        assert!(info.execution_diagnostics.tracks_tool_verification);
        assert!(info.execution_diagnostics.tracks_branch_contradiction_analysis);
        assert!(info.execution_diagnostics.tracks_learned_route_trends);
        assert!(info
            .execution_diagnostics
            .worker_stage_reporting
            .contains(&"escalation".to_string()));
    }

    #[test]
    fn test_default_system_prompt_keeps_ngenorca_identity() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);

        let messages = orch.build_messages("", &[], "Olá", None, None, None);
        let system = &messages[0].content;

        assert!(system.contains("You are NgenOrca"));
        assert!(!system.contains("You are a helpful assistant."));
    }

    #[test]
    fn test_default_system_prompt_lists_available_tools() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let tools = vec![ToolDefinition {
            name: "read_file".into(),
            description: "Read a file from the workspace".into(),
            parameters: serde_json::json!({"type": "object"}),
            requires_sandbox: false,
        }];

        let messages = orch.build_messages("", &[], "Inspect the repo", Some(&tools), None, None);
        let system = &messages[0].content;

        assert!(system.contains("Available tools"));
        assert!(system.contains("read_file"));
    }

    #[test]
    fn test_worker_contract_preserves_primary_identity() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config.clone());
        let agent = &config.agent.sub_agents[1];
        let classification = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: TaskComplexity::Moderate,
            confidence: 0.95,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["rust".into(), "tooling".into()],
            language: Some("en".into()),
        };

        let prompt = orch.generate_system_prompt(agent, &classification, Some("en"));
        assert!(prompt.contains("delegated specialist working on behalf of NgenOrca"));
        assert!(prompt.contains("Do not present yourself as a separate assistant"));
        assert!(prompt.contains("Focus domains: rust, tooling."));
    }

    #[test]
    fn test_build_delegation_plan_for_complex_task() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let classification = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: TaskComplexity::Complex,
            confidence: 0.91,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["rust".into()],
            language: Some("en".into()),
        };
        let routing = orch.route(&classification);

        let plan = orch.build_delegation_plan(&classification, &routing).unwrap();
        assert_eq!(plan.strategy, "parallel-framing-and-execution");
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].agent.name, "deep-thinker");
        assert_eq!(plan.steps[1].agent.name, routing.target.name);
    }

    #[test]
    fn test_build_delegation_plan_for_moderate_task_adds_cross_check() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let classification = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: TaskComplexity::Moderate,
            confidence: 0.91,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["rust".into()],
            language: Some("en".into()),
        };
        let routing = orch.route(&classification);

        let plan = orch.build_delegation_plan(&classification, &routing).unwrap();
        assert_eq!(plan.strategy, "parallel-multi-branch");
        assert_eq!(plan.steps.len(), 4);
        assert_eq!(plan.steps[0].agent.name, "deep-thinker");
        assert_eq!(plan.steps[1].agent.name, "local-general");
        assert_eq!(plan.steps[2].agent.name, routing.target.name);
    }

    #[test]
    fn test_build_messages_include_structured_plan() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let plan = DelegationPlan {
            strategy: "structured-sequential".into(),
            steps: vec![DelegationPlanStep {
                id: "frame-task".into(),
                goal: "Clarify constraints first.".into(),
                agent: SubAgentId {
                    name: "deep-thinker".into(),
                    model: "anthropic/claude-sonnet-4-20250514".into(),
                },
            }],
        };

        let messages = orch.build_messages("", &[], "Plan the migration", None, None, Some(&plan));
        assert!(messages[0].content.contains("Structured execution plan"));
        assert!(messages[0].content.contains("frame-task via deep-thinker"));
    }

    #[test]
    fn test_parallel_support_steps_pick_all_non_primary_parallel_branches() {
        let plan = DelegationPlan {
            strategy: "parallel-multi-branch".into(),
            steps: vec![
                DelegationPlanStep {
                    id: "frame-task".into(),
                    goal: "Clarify constraints first.".into(),
                    agent: SubAgentId {
                        name: "deep-thinker".into(),
                        model: "anthropic/claude-sonnet-4-20250514".into(),
                    },
                },
                DelegationPlanStep {
                    id: "execute-domain-work".into(),
                    goal: "Implement the requested fix.".into(),
                    agent: SubAgentId {
                        name: "coder".into(),
                        model: "ollama/codellama:13b".into(),
                    },
                },
                DelegationPlanStep {
                    id: "cross-check".into(),
                    goal: "Review risks in parallel.".into(),
                    agent: SubAgentId {
                        name: "local-general".into(),
                        model: "ollama/llama3.1:8b".into(),
                    },
                },
            ],
        };

        let steps = parallel_support_steps(
            &plan,
            &SubAgentId {
                name: "coder".into(),
                model: "ollama/codellama:13b".into(),
            },
        );

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].id, "frame-task");
        assert_eq!(steps[0].agent.name, "deep-thinker");
        assert_eq!(steps[1].id, "cross-check");
        assert_eq!(steps[1].agent.name, "local-general");
    }

    #[test]
    fn test_build_parallel_support_request_limits_scope() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let classification = TaskClassification {
            intent: TaskIntent::Analysis,
            complexity: TaskComplexity::Complex,
            confidence: 0.9,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["logs".into()],
            language: Some("en".into()),
        };
        let step = DelegationPlanStep {
            id: "frame-task".into(),
            goal: "Clarify failure hypotheses before deeper execution.".into(),
            agent: SubAgentId {
                name: "deep-thinker".into(),
                model: "anthropic/claude-sonnet-4-20250514".into(),
            },
        };

        let memory_context = ngenorca_memory::ContextPack {
            semantic_block: vec![
                ngenorca_memory::semantic::SemanticFact {
                    id: 1,
                    user_id: "user-1".into(),
                    category: ngenorca_memory::semantic::FactCategory::Goal,
                    fact: "Needs a deployment failure triage plan".into(),
                    confidence: 0.95,
                    source_episode_ids: vec![],
                    established_at: chrono::Utc::now(),
                    last_confirmed: chrono::Utc::now(),
                    access_count: 4,
                },
                ngenorca_memory::semantic::SemanticFact {
                    id: 2,
                    user_id: "user-1".into(),
                    category: ngenorca_memory::semantic::FactCategory::Preference,
                    fact: "Prefers concise rollout notes".into(),
                    confidence: 0.88,
                    source_episode_ids: vec![],
                    established_at: chrono::Utc::now(),
                    last_confirmed: chrono::Utc::now(),
                    access_count: 2,
                },
            ],
            episodic_snippets: vec![ngenorca_memory::episodic::EpisodicEntry {
                id: 10,
                user_id: "user-1".into(),
                content: "Last outage came from a missing environment variable in production.".into(),
                summary: Some("Earlier outage caused by missing environment variable.".into()),
                channel: "web".into(),
                timestamp: chrono::Utc::now(),
                embedding: Some(vec![]),
                relevance_score: 0.92,
            }],
            working_messages: vec![ngenorca_memory::working::WorkingMessage {
                role: "user".into(),
                content: "Check likely causes before touching code.".into(),
                timestamp: chrono::Utc::now(),
                estimated_tokens: 10,
            }],
            total_estimated_tokens: 32,
        };

        let (request, memory_view) = orch.build_parallel_support_request(
            &classification,
            &step,
            &[],
            "Why is the service failing?",
            Some(&memory_context),
            None,
        );

        assert_eq!(request.model, "anthropic/claude-sonnet-4-20250514");
        assert!(request.tools.is_none());
        assert_eq!(memory_view.unwrap().memory_scope, "goal-and-constraint slice");
        assert!(request.messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("parallel support branch")
                && m.content.contains("Step id: frame-task")
                && m.content.contains("do not present this as the final user answer")
        }));
        assert!(request.messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("Branch memory scope: goal-and-constraint slice")
                && m.content.contains("Evidence focus:")
                && m.content.contains("semantic::goal::Needs a deployment failure triage plan")
                && m.content.contains("working::user::Check likely causes before touching code.")
        }));
    }

    #[test]
    fn test_format_tool_feedback_marks_failures_and_retryability() {
        let tc = ToolCallResponse {
            id: "call-1".into(),
            name: "run_command".into(),
            arguments: serde_json::json!({"command": "cargo"}),
        };

        let blocked = format_tool_feedback(
            &tc,
            &ToolExecutionOutcome::Blocked {
                error: "blocked".into(),
            },
        );
        assert!(blocked.contains("\"ok\": false"));
        assert!(blocked.contains("\"retryable\": false"));

        let failed = format_tool_feedback(
            &tc,
            &ToolExecutionOutcome::Failed {
                error: "boom".into(),
            },
        );
        assert!(failed.contains("\"retryable\": true"));
    }

    #[test]
    fn test_tool_call_signature_ignores_call_id() {
        let tc1 = ToolCallResponse {
            id: "call-a".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        };
        let tc2 = ToolCallResponse {
            id: "call-b".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        };

        assert_eq!(tool_call_signature(&tc1), tool_call_signature(&tc2));
    }

    #[test]
    fn test_should_primary_synthesize_only_for_workers() {
        assert!(should_primary_synthesize(&SubAgentId {
            name: "coder".into(),
            model: "ollama/codellama".into(),
        }));
        assert!(!should_primary_synthesize(&SubAgentId {
            name: "primary".into(),
            model: "anthropic/claude-sonnet-4-20250514".into(),
        }));
    }

    #[test]
    fn test_build_synthesis_messages_preserves_primary_ownership() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let classification = TaskClassification {
            intent: TaskIntent::Coding,
            complexity: TaskComplexity::Moderate,
            confidence: 0.9,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["rust".into(), "workspace".into()],
            language: Some("en".into()),
        };
        let drafts = vec![SpecialistDraft {
            stage: "initial".into(),
            agent: SubAgentId {
                name: "coder".into(),
                model: "ollama/codellama:13b".into(),
            },
            content: "Worker draft: update Cargo.toml and rebuild.".into(),
            note: Some("Initial coding pass".into()),
            branch_role: "execution".into(),
            reliability: "working".into(),
            priority_weight: 70,
            memory_scope: Some("execution slice".into()),
            evidence_focus: Some("Prioritize technically actionable preferences.".into()),
            evidence_items: vec!["working::user::Fix the build before release".into()],
        }];

        let messages = orch.build_synthesis_messages(
            &classification,
            &[],
            "Fix the build issue",
            &drafts,
            "Worker draft: update Cargo.toml and rebuild.",
            None,
            None,
        );

        assert!(messages.iter().any(|m| m.role == "assistant" && m.content.contains("Worker draft")));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("Specialist draft history")
                && m.content.contains("Stage: initial")
                && m.content.contains("Reliability: working")
                && m.content.contains("Memory scope: execution slice")
                && m.content.contains("Evidence slice:")
                && m.content.contains("Initial coding pass")
        }));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("Branch reconciliation policy")
                && m.content.contains("grounded or corrected execution draft")
                && m.content.contains("role=execution, reliability=working, priority=70")
        }));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("final answer as NgenOrca")
                && m.content.contains("without mentioning delegation")
                && m.content.contains("Reconcile the specialist drafts")
                && m.content.contains("Grounded execution drafts outrank advisory support branches")
                && m.content.contains("Use each branch's evidence slice")
                && m.content.contains("rust, workspace")
        }));
    }

    #[test]
    fn test_build_escalation_messages_include_handoff_context() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config.clone());
        let classification = TaskClassification {
            intent: TaskIntent::Analysis,
            complexity: TaskComplexity::Complex,
            confidence: 0.81,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["logs".into()],
            language: Some("en".into()),
        };

        let messages = orch.build_escalation_messages(
            &classification,
            "Escalation prompt",
            &[],
            "Why is the service failing?",
            "Initial worker draft",
            &SubAgentId {
                name: "local-general".into(),
                model: "ollama/llama3.1:8b".into(),
            },
            "response was too shallow",
            None,
            None,
            None,
        );

        assert!(messages.iter().any(|m| m.role == "assistant" && m.content == "Initial worker draft"));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("taking over from a previous specialist draft")
                && m.content.contains("local-general/ollama/llama3.1:8b")
                && m.content.contains("response was too shallow")
        }));
    }

    #[test]
    fn test_build_augmentation_messages_include_gap_context() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config.clone());
        let classification = TaskClassification {
            intent: TaskIntent::Planning,
            complexity: TaskComplexity::Moderate,
            confidence: 0.84,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["deployment".into()],
            language: Some("en".into()),
        };

        let messages = orch.build_augmentation_messages(
            &classification,
            "Augment prompt",
            &[],
            "Plan the rollout",
            "Partial rollout plan",
            "rollback and verification steps",
            &SubAgentId {
                name: "deep-thinker".into(),
                model: "anthropic/claude-sonnet-4-20250514".into(),
            },
            None,
            None,
            None,
        );

        assert!(messages.iter().any(|m| m.role == "assistant" && m.content == "Partial rollout plan"));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("revising your own earlier specialist draft")
                && m.content.contains("rollback and verification steps")
        }));
        assert!(messages.iter().any(|m| {
            m.role == "user"
                && m.content.contains("fully covers: rollback and verification steps")
        }));
    }

    #[test]
    fn test_specialist_draft_history_preserves_multiple_stages() {
        let drafts = vec![
            SpecialistDraft {
                stage: "initial".into(),
                agent: SubAgentId {
                    name: "coder".into(),
                    model: "ollama/codellama".into(),
                },
                content: "First draft".into(),
                note: Some("Initial attempt".into()),
                branch_role: "execution".into(),
                reliability: "working".into(),
                priority_weight: 70,
                memory_scope: Some("execution slice".into()),
                evidence_focus: Some("Keep the latest active repo context.".into()),
                evidence_items: vec!["working::user::Fix the failing tests".into()],
            },
            SpecialistDraft {
                stage: "escalation".into(),
                agent: SubAgentId {
                    name: "deep-thinker".into(),
                    model: "anthropic/claude-sonnet-4".into(),
                },
                content: "Improved draft".into(),
                note: None,
                branch_role: "execution".into(),
                reliability: "corrected".into(),
                priority_weight: 90,
                memory_scope: Some("execution slice".into()),
                evidence_focus: Some("Check the same execution evidence with a stronger model.".into()),
                evidence_items: vec!["episodic::2025-01-01::Prior build failed after dependency bump".into()],
            },
        ];

        let history = specialist_draft_history(&drafts, "Verified draft");
        assert!(history.contains("Stage: initial"));
        assert!(history.contains("Stage: escalation"));
        assert!(history.contains("Stage: latest_worker_response"));
        assert!(history.contains("Role: execution"));
        assert!(history.contains("Reliability: corrected"));
        assert!(history.contains("Initial attempt"));
        assert!(history.contains("Memory scope: execution slice"));
        assert!(history.contains("Evidence slice:"));
        assert!(history.contains("Verified draft"));
    }

    #[test]
    fn test_build_tool_verification_messages_include_tool_report() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let classification = TaskClassification {
            intent: TaskIntent::ToolUse,
            complexity: TaskComplexity::Moderate,
            confidence: 0.91,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["workspace".into()],
            language: Some("en".into()),
        };
        let summary = ToolLoopSummary {
            rounds: 1,
            tool_names: vec!["run_command".into()],
            tool_feedback: vec![ChatMessage {
                role: "tool".into(),
                content: "tool feedback body".into(),
            }],
            tool_observations: vec![],
            had_failures: true,
            had_blocked_calls: false,
            retry_guidance: vec!["Inspect stdout/stderr before retrying once.".into()],
            attempt_trace: vec![],
        };

        let messages = orch.build_tool_verification_messages(
            &classification,
            &[],
            "Check the build",
            "Draft answer",
            &summary,
            None,
        );

        assert!(messages.iter().any(|m| m.role == "assistant" && m.content == "Draft answer"));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("grounded in the observed tool results")
                && m.content.contains("run_command")
                && m.content.contains("tool feedback body")
                && m.content.contains("Inspect stdout/stderr before retrying once")
        }));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("Return JSON only")
                && m.content.contains("should_retry_tools")
        }));
    }

    #[test]
    fn test_build_tool_remediation_messages_include_correction_report() {
        let config = Arc::new(test_config());
        let orch = HybridOrchestrator::new(config);
        let classification = TaskClassification {
            intent: TaskIntent::ToolUse,
            complexity: TaskComplexity::Complex,
            confidence: 0.88,
            method: ClassificationMethod::RuleBased,
            domain_tags: vec!["workspace".into()],
            language: Some("en".into()),
        };
        let summary = ToolLoopSummary {
            rounds: 1,
            tool_names: vec!["write_file".into()],
            tool_feedback: vec![ChatMessage {
                role: "tool".into(),
                content: "write feedback".into(),
            }],
            tool_observations: vec![],
            had_failures: false,
            had_blocked_calls: false,
            retry_guidance: vec!["Read the file back before finalizing.".into()],
            attempt_trace: vec![],
        };
        let report = ToolVerificationReport {
            grounded: false,
            should_retry_tools: true,
            corrected_answer: "Fallback answer".into(),
            retry_instruction: Some("Use read_file to confirm the write.".into()),
            issues: vec!["write not confirmed".into()],
        };

        let messages = orch.build_tool_remediation_messages(
            &classification,
            &[],
            "Update the config",
            "Draft answer",
            &summary,
            &report,
            None,
            None,
        );

        assert!(messages.iter().any(|m| m.role == "assistant" && m.content == "Draft answer"));
        assert!(messages.iter().any(|m| {
            m.role == "system"
                && m.content.contains("still needs one corrective pass")
                && m.content.contains("Use read_file to confirm the write")
                && m.content.contains("write feedback")
        }));
    }

    #[test]
    fn test_parse_tool_verification_report_handles_json_and_fallbacks() {
        let summary = ToolLoopSummary {
            rounds: 1,
            tool_names: vec!["write_file".into()],
            tool_feedback: vec![],
            tool_observations: vec![],
            had_failures: false,
            had_blocked_calls: false,
            retry_guidance: vec!["Read back the file once.".into()],
            attempt_trace: vec![],
        };

        let parsed = parse_tool_verification_report(
            Some(
                "```json\n{\"grounded\":false,\"should_retry_tools\":true,\"corrected_answer\":\"Need to verify\",\"retry_instruction\":\"Use read_file\",\"issues\":[\"write not confirmed\"]}\n```",
            ),
            "fallback",
            &summary,
        );
        assert!(!parsed.grounded);
        assert!(parsed.should_retry_tools);
        assert_eq!(parsed.corrected_answer, "Need to verify");
        assert_eq!(parsed.retry_instruction.as_deref(), Some("Use read_file"));

        let fallback = parse_tool_verification_report(Some("plain corrected answer"), "fallback", &summary);
        assert_eq!(fallback.corrected_answer, "plain corrected answer");
        assert!(fallback.should_retry_tools);
    }

    #[test]
    fn test_tool_verified_response_uses_corrected_answer() {
        let response = ChatCompletionResponse {
            content: Some("Draft".into()),
            tool_calls: vec![],
            usage: Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            },
        };
        let report = ToolVerificationReport {
            grounded: true,
            should_retry_tools: false,
            corrected_answer: "Corrected".into(),
            retry_instruction: None,
            issues: vec![],
        };

        let verified = tool_verified_response(&response, &report);
        assert_eq!(verified.content.as_deref(), Some("Corrected"));
        assert_eq!(verified.usage.total_tokens, 3);
    }

    #[test]
    fn test_verification_diagnostics_keeps_operator_fields() {
        let report = ToolVerificationReport {
            grounded: false,
            should_retry_tools: true,
            corrected_answer: "answer".into(),
            retry_instruction: Some("Use read_file".into()),
            issues: vec!["write not confirmed".into()],
        };

        let diagnostics = verification_diagnostics(&report);
        assert!(!diagnostics.grounded);
        assert!(diagnostics.should_retry_tools);
        assert_eq!(diagnostics.retry_instruction.as_deref(), Some("Use read_file"));
        assert_eq!(diagnostics.issues, vec!["write not confirmed"]);
    }

    #[test]
    fn test_tool_loop_summary_merge_accumulates_flags() {
        let mut a = ToolLoopSummary {
            rounds: 1,
            tool_names: vec!["read_file".into()],
            tool_feedback: vec![],
            tool_observations: vec![],
            had_failures: false,
            had_blocked_calls: false,
            retry_guidance: vec![],
            attempt_trace: vec![CorrectionAttemptTrace {
                round: 1,
                tool: "read_file".into(),
                outcome: "success".into(),
                failure_class: None,
                guidance: None,
            }],
        };
        let b = ToolLoopSummary {
            rounds: 2,
            tool_names: vec!["run_command".into()],
            tool_feedback: vec![ChatMessage {
                role: "tool".into(),
                content: "feedback".into(),
            }],
            tool_observations: vec![],
            had_failures: true,
            had_blocked_calls: true,
            retry_guidance: vec!["Inspect stderr before retrying once.".into()],
            attempt_trace: vec![CorrectionAttemptTrace {
                round: 2,
                tool: "run_command".into(),
                outcome: "failed".into(),
                failure_class: Some("execution".into()),
                guidance: Some("Inspect stderr before retrying once.".into()),
            }],
        };

        a.merge(b);
        assert_eq!(a.rounds, 3);
        assert_eq!(a.tool_names.len(), 2);
        assert_eq!(a.tool_feedback.len(), 1);
        assert!(a.had_failures);
        assert!(a.had_blocked_calls);
        assert_eq!(a.retry_guidance.len(), 1);
        assert_eq!(a.attempt_trace.len(), 2);
    }

    #[test]
    fn test_contradiction_scan_flags_conflicting_support_branch() {
        let drafts = vec![
            SpecialistDraft {
                stage: "verified".into(),
                agent: SubAgentId {
                    name: "coder".into(),
                    model: "ollama/codellama".into(),
                },
                content: "Update Cargo.toml, rerun cargo test, and keep the dependency version pinned.".into(),
                note: None,
                branch_role: "execution".into(),
                reliability: "grounded".into(),
                priority_weight: 100,
                memory_scope: None,
                evidence_focus: None,
                evidence_items: vec![],
            },
            SpecialistDraft {
                stage: "parallel-support".into(),
                agent: SubAgentId {
                    name: "deep-thinker".into(),
                    model: "anthropic/claude-sonnet-4".into(),
                },
                content: "However, do not pin the dependency; instead revert the update and avoid running cargo test until the lockfile is regenerated.".into(),
                note: None,
                branch_role: "support".into(),
                reliability: "advisory".into(),
                priority_weight: 40,
                memory_scope: None,
                evidence_focus: None,
                evidence_items: vec![],
            },
        ];

        let scan = contradiction_scan(&drafts);
        assert!(scan.score >= 0.45);
        assert_eq!(scan.conflicting_branches, 1);
        assert_eq!(scan.anchor_stage.as_deref(), Some("verified"));
        assert!(scan
            .signals
            .iter()
            .any(|signal| signal.contains("negated_action_overlap")));
        assert!(scan.summary[0].contains("parallel-support"));
    }

    #[test]
    fn test_contradiction_scan_detects_numeric_mismatch() {
        let drafts = vec![
            SpecialistDraft {
                stage: "execute-domain-work".into(),
                agent: SubAgentId {
                    name: "coder".into(),
                    model: "ollama/codellama".into(),
                },
                content: "Set the timeout to 30 seconds and keep retry count at 2.".into(),
                note: None,
                branch_role: "execution".into(),
                reliability: "grounded".into(),
                priority_weight: 90,
                memory_scope: None,
                evidence_focus: None,
                evidence_items: vec![],
            },
            SpecialistDraft {
                stage: "cross-check".into(),
                agent: SubAgentId {
                    name: "deep-thinker".into(),
                    model: "anthropic/claude-sonnet-4".into(),
                },
                content: "However, set the timeout to 45 seconds and use retry count 4 instead.".into(),
                note: None,
                branch_role: "support".into(),
                reliability: "advisory".into(),
                priority_weight: 40,
                memory_scope: None,
                evidence_focus: None,
                evidence_items: vec![],
            },
        ];

        let scan = contradiction_scan(&drafts);
        assert_eq!(scan.conflicting_branches, 1);
        assert!(scan.signals.iter().any(|signal| signal == "numeric_mismatch"));
    }

    #[test]
    fn test_tool_loop_summary_detects_pending_write_verification() {
        let mut summary = ToolLoopSummary::default();
        summary.tool_names.push("write_file".into());
        assert!(summary.needs_write_verification());

        summary.tool_names.push("read_file".into());
        assert!(!summary.needs_write_verification());
    }

    #[test]
    fn test_tool_loop_summary_detects_post_edit_command_verification_gap() {
        let summary = ToolLoopSummary {
            rounds: 2,
            tool_names: vec!["write_file".into(), "run_command".into(), "write_file".into()],
            tool_feedback: vec![],
            tool_observations: vec![
                ToolObservation {
                    tool: "write_file".into(),
                    ok: true,
                    retryable: false,
                    result: Some(serde_json::json!({"path": "src/lib.rs"})),
                    error: None,
                },
                ToolObservation {
                    tool: "run_command".into(),
                    ok: true,
                    retryable: false,
                    result: Some(serde_json::json!({
                        "command": "cargo",
                        "args": ["test"],
                        "exit_code": 0,
                        "success": true,
                        "timed_out": false
                    })),
                    error: None,
                },
                ToolObservation {
                    tool: "write_file".into(),
                    ok: true,
                    retryable: false,
                    result: Some(serde_json::json!({"path": "src/lib.rs"})),
                    error: None,
                },
            ],
            had_failures: false,
            had_blocked_calls: false,
            retry_guidance: vec![],
            attempt_trace: vec![],
        };

        assert!(summary.needs_command_verification());
        assert!(summary.requires_follow_up_verification());
        assert!(summary
            .follow_up_verification_instruction()
            .is_some_and(|instruction| instruction.contains("focused verification command")));
    }

    #[test]
    fn test_tool_loop_summary_escalates_after_repeated_command_failures() {
        let summary = ToolLoopSummary {
            rounds: 2,
            tool_names: vec!["run_command".into(), "run_command".into()],
            tool_feedback: vec![],
            tool_observations: vec![
                ToolObservation {
                    tool: "run_command".into(),
                    ok: true,
                    retryable: false,
                    result: Some(serde_json::json!({
                        "command": "cargo",
                        "args": ["test"],
                        "exit_code": 101,
                        "success": false,
                        "timed_out": false
                    })),
                    error: None,
                },
                ToolObservation {
                    tool: "run_command".into(),
                    ok: true,
                    retryable: false,
                    result: Some(serde_json::json!({
                        "command": "cargo",
                        "args": ["test"],
                        "exit_code": 101,
                        "success": false,
                        "timed_out": false
                    })),
                    error: None,
                },
            ],
            had_failures: false,
            had_blocked_calls: true,
            retry_guidance: vec!["Inspect stderr before retrying once.".into()],
            attempt_trace: vec![
                CorrectionAttemptTrace {
                    round: 1,
                    tool: "run_command".into(),
                    outcome: "success".into(),
                    failure_class: Some("execution".into()),
                    guidance: Some("Inspect stderr before retrying once.".into()),
                },
                CorrectionAttemptTrace {
                    round: 2,
                    tool: "run_command".into(),
                    outcome: "success".into(),
                    failure_class: Some("execution".into()),
                    guidance: Some("Inspect stderr before retrying once.".into()),
                },
            ],
        };

        assert_eq!(summary.repeated_failure_tools(), vec!["run_command".to_string()]);
        assert!(summary.should_abandon_tool_retries());
        assert!(summary
            .retry_instruction()
            .is_some_and(|instruction| instruction.contains("stop retrying the same recovery path")));
    }

    #[test]
    fn test_parse_tool_verification_report_uses_default_workflow_issues_when_needed() {
        let summary = ToolLoopSummary {
            rounds: 2,
            tool_names: vec!["write_file".into(), "run_command".into()],
            tool_feedback: vec![],
            tool_observations: vec![
                ToolObservation {
                    tool: "write_file".into(),
                    ok: true,
                    retryable: false,
                    result: Some(serde_json::json!({"path": "src/main.rs"})),
                    error: None,
                },
                ToolObservation {
                    tool: "run_command".into(),
                    ok: true,
                    retryable: false,
                    result: Some(serde_json::json!({
                        "command": "cargo",
                        "args": ["test"],
                        "exit_code": 101,
                        "success": false,
                        "timed_out": false
                    })),
                    error: None,
                },
            ],
            had_failures: false,
            had_blocked_calls: false,
            retry_guidance: vec![],
            attempt_trace: vec![],
        };

        let report = parse_tool_verification_report(
            Some("```json\n{\"grounded\":false,\"should_retry_tools\":true,\"corrected_answer\":\"Need more verification\",\"retry_instruction\":null,\"issues\":[]}\n```"),
            "fallback",
            &summary,
        );

        assert!(!report.grounded);
        assert!(report.should_retry_tools);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.contains("verification command 'cargo' exited with code 101")));
    }

    #[test]
    fn test_tool_retry_guidance_is_domain_specific() {
        let failed_command = tool_retry_guidance(
            "run_command",
            &ToolExecutionOutcome::Failed {
                error: "command failed".into(),
            },
        )
        .unwrap();
        assert!(failed_command.contains("stdout/stderr"));

        let blocked_write = tool_retry_guidance(
            "write_file",
            &ToolExecutionOutcome::Blocked {
                error: "duplicate call".into(),
            },
        )
        .unwrap();
        assert!(blocked_write.contains("Do not repeat"));
    }
}
