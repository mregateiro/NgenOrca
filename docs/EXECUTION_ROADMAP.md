# NgenOrca Execution Roadmap

This roadmap translates the OpenClaw comparison into concrete workstreams, acceptance criteria, and a parallel execution model.

## Roadmap intent

The goal is not to imitate OpenClaw superficially. The goal is to make NgenOrca operationally coherent and capable:

- strong assistant identity
- correct use of memory
- coordinated sub-agents
- safe and reliable tools
- self-correction loops
- reusable skills and automations

## Execution principles

- prefer small, testable changes
- never fix one path without checking its siblings
- treat HTTP, WebSocket, and background worker as parity-critical
- keep the primary assistant as the single owner of identity and continuity
- treat sub-agents as delegated workers
- never declare a feature complete until the read path and write path both work

## Epic 1: Runtime parity and unified identity

### Goal
Make all equivalent runtime surfaces behave consistently and attach requests to a coherent user/session identity.

### Tasks
- [ ] align session semantics across HTTP, WebSocket, and channel worker paths
- [ ] support explicit session reuse consistently where applicable
- [ ] ensure episodic writes happen consistently across all chat entrypoints
- [ ] eliminate prompt-construction mismatches between entrypoints
- [ ] wire unified identity resolution into live serving flows
- [ ] verify cross-channel user continuity rules

### Acceptance criteria
- same user gets coherent memory/session behavior across all entrypoints
- no path skips memory writes or prompt context unexpectedly
- runtime identity is stable enough to support real cross-channel memory

### Primary files/flows
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/src/lib.rs](crates/ngenorca-gateway/src/lib.rs)
- [crates/ngenorca-gateway/src/sessions.rs](crates/ngenorca-gateway/src/sessions.rs)
- [crates/ngenorca-identity/src/lib.rs](crates/ngenorca-identity/src/lib.rs)
- [crates/ngenorca-identity/src/resolver.rs](crates/ngenorca-identity/src/resolver.rs)

## Epic 2: Intent-aware memory retrieval

### Goal
Make memory retrieval depend on task intent, user identity, and domain context.

### Tasks
- [ ] define memory retrieval policies per `TaskIntent`
- [ ] add retrieval weighting for semantic vs episodic vs working memory
- [ ] use `domain_tags` and classification metadata in retrieval
- [ ] improve coding-task memory retrieval around technical preferences and repo history
- [ ] improve planning/analysis retrieval for broader prior context
- [ ] reduce memory load for lightweight intents like simple Q&A
- [ ] add tests proving retrieval strategy differs by intent

### Acceptance criteria
- memory retrieval strategy changes based on task type
- user-relevant memory is consistently available in subsequent interactions
- memory token use is intentional rather than generic

### Primary files/flows
- [crates/ngenorca-memory/src/lib.rs](crates/ngenorca-memory/src/lib.rs)
- [crates/ngenorca-memory/src/episodic.rs](crates/ngenorca-memory/src/episodic.rs)
- [crates/ngenorca-memory/src/semantic.rs](crates/ngenorca-memory/src/semantic.rs)
- [crates/ngenorca-gateway/src/orchestration/classifier.rs](crates/ngenorca-gateway/src/orchestration/classifier.rs)
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

## Epic 3: Learned routing that actually routes

### Goal
Turn learned routing from passive analytics into active decision support.

### Tasks
- [ ] consult learned rules during routing
- [ ] apply confidence weighting and fallback safely
- [ ] incorporate domain and intent specificity in learned lookups
- [ ] penalize routes that repeatedly lead to escalation or augmentation
- [ ] surface learned-route usage in runtime diagnostics
- [ ] test successful and failed learned-route reuse

### Acceptance criteria
- successful past routes can influence future routing decisions
- repeated failures reduce route reuse
- `from_memory` reflects real learned-route decisions

### Primary files/flows
- [crates/ngenorca-gateway/src/orchestration/learned.rs](crates/ngenorca-gateway/src/orchestration/learned.rs)
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)

## Epic 4: Primary-agent ownership and delegated worker sub-agents

### Goal
Keep one strong top-level assistant while turning sub-agents into focused workers.

### Tasks
- [ ] define delegation contract from primary assistant to worker agents
- [ ] pass only the necessary memory/context to each worker
- [ ] require structured worker outputs for synthesis
- [ ] prevent worker prompts from replacing the primary identity
- [ ] support multi-step decomposition for complex tasks
- [ ] add explicit tests for delegated-worker behavior

### Acceptance criteria
- primary assistant remains the visible owner of conversation
- worker agents can help without fragmenting identity or memory continuity
- complex tasks can be decomposed with explicit handoff boundaries

### Primary files/flows
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-config/src/lib.rs](crates/ngenorca-config/src/lib.rs)
- [config/config.example.toml](config/config.example.toml)

## Epic 5: Tool execution, verification, and safe autonomy

### Goal
Make tools reliable enough for self-directed work.

### Tasks
- [ ] preserve session and user context during tool execution
- [ ] publish tool execution telemetry/events
- [ ] enforce real sandbox behavior for sandbox-required tools
- [ ] add tool-result verification/retry patterns
- [ ] add failure-aware command/file workflows
- [ ] standardize how tools report machine-readable outcomes

### Acceptance criteria
- tool calls are contextualized, auditable, and safer
- failed tool runs can be retried or corrected automatically
- automation work can rely on tools instead of brittle prompting

### Primary files/flows
- [crates/ngenorca-gateway/src/plugins.rs](crates/ngenorca-gateway/src/plugins.rs)
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-sandbox/src/lib.rs](crates/ngenorca-sandbox/src/lib.rs)

## Epic 6: Self-correction loops beyond one pass

### Goal
Move from simple escalation/augmentation to plan-act-verify-correct loops.

### Tasks
- [ ] define iterative correction loop boundaries
- [ ] add explicit verification checkpoints after tool use and complex outputs
- [ ] add retry strategy before escalation where appropriate
- [ ] distinguish operational failure from reasoning failure
- [ ] cap loops safely to avoid runaway behavior

### Acceptance criteria
- the assistant can detect and correct some failures before giving up
- loop limits are explicit and observable
- self-correction improves outcomes without masking failures

### Primary files/flows
- [crates/ngenorca-gateway/src/orchestration/quality.rs](crates/ngenorca-gateway/src/orchestration/quality.rs)
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

## Epic 7: Skills and automations as first-class capabilities

### Goal
Let NgenOrca produce and reuse skills, scripts, and automations in a structured way.

### Tasks
- [ ] define what a skill is in NgenOrca terms
- [ ] define how a skill declares tools, constraints, and inputs
- [ ] add reusable task recipes for common development and ops workflows
- [ ] add script-generation workflows with validation
- [ ] add automation primitives for recurring actions
- [ ] document operator review boundaries for generated automations

### Acceptance criteria
- skills are explicit artifacts, not just loose prompting
- generated scripts/automations follow repeatable validation rules
- the platform can accumulate reusable operational knowledge

### Primary files/flows
- [crates/ngenorca-plugin-sdk/src/lib.rs](crates/ngenorca-plugin-sdk/src/lib.rs)
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [docs/CONFIGURATION_GUIDE.md](CONFIGURATION_GUIDE.md)
- [README.md](../README.md)

## Parallel sub-agent work model

The following workstreams can run in parallel, with one coordinating lead reviewer.

### Lead reviewer/orchestrator
Responsibilities:
- maintain system invariants
- compare equivalent flows
- approve merge order
- watch for hidden cross-cutting regressions

### Subagent A: runtime-parity auditor
Scope:
- Epic 1
- session, channel, identity, HTTP/WS/worker parity

Deliverables:
- path map
- inconsistency list
- minimal fixes with regression checks

### Subagent B: memory designer
Scope:
- Epic 2
- memory retrieval and consolidation strategy

Deliverables:
- intent-aware retrieval policy
- token-budget rules
- memory read/write parity checks

### Subagent C: routing and worker-agent designer
Scope:
- Epic 3 and Epic 4
- learned routing and delegated worker contract

Deliverables:
- learned-route integration plan
- worker handoff contract
- primary/worker role separation rules

### Subagent D: tool autonomy and safety engineer
Scope:
- Epic 5 and Epic 6
- tool context, sandboxing, verification, retries

Deliverables:
- tool execution contract
- verification loop design
- audit/event model

### Subagent E: skills and automation designer
Scope:
- Epic 7
- skill artifacts, script generation, automation lifecycle

Deliverables:
- skill schema proposal
- script/automation workflow proposal
- operator safety boundaries

## Recommended implementation order

1. Epic 1
2. Epic 2
3. Epic 3
4. Epic 4
5. Epic 5
6. Epic 6
7. Epic 7

This order is deliberate:
- parity and identity first
- memory and learned behavior second
- cooperative sub-agents third
- stronger autonomy last

## Definition of done for the roadmap

The roadmap should only be considered complete when:

- all equivalent runtime entrypoints behave coherently
- memory is actively used according to intent and user context
- learned routing influences real decisions
- the primary assistant keeps identity ownership while workers assist
- tools run with context, auditability, and safer correction loops
- skills and automation workflows are reusable and validated
