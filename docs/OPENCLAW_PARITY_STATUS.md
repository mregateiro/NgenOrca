# NgenOrca OpenClaw Parity Status

Last updated: 2026-03-15

This file tracks what has already been implemented locally toward functional parity with OpenClaw, what was validated, and what risks or gaps remain.

It is intended to be the running operational status file, while the broader comparison and plan remain in:

- [docs/OPENCLAW_GAP_ANALYSIS.md](docs/OPENCLAW_GAP_ANALYSIS.md)
- [docs/EXECUTION_ROADMAP.md](docs/EXECUTION_ROADMAP.md)

## Completed locally so far

### 1. Assistant identity and capability framing

Implemented:
- strengthened the primary system prompt so NgenOrca identifies itself as the assistant layer rather than a raw model passthrough
- made the prompt capability-aware so tool use is described as an available runtime capability
- preserved sub-agent role instructions as an add-on instead of replacing NgenOrca's identity

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- prompt-focused unit tests in the gateway crate

### 2. Memory parity across runtime entrypoints

Implemented:
- aligned HTTP, WebSocket, and inbound worker paths so all classify before retrieval
- added task-aware memory retrieval via `build_context_for_task(...)`
- ensured WebSocket also persists episodic memory
- ensured memory context includes prior working memory consistently
- made episodic retrieval respect token budget trimming

Main files:
- [crates/ngenorca-memory/src/lib.rs](crates/ngenorca-memory/src/lib.rs)
- [crates/ngenorca-memory/src/episodic.rs](crates/ngenorca-memory/src/episodic.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/src/lib.rs](crates/ngenorca-gateway/src/lib.rs)

Validated:
- memory crate tests
- gateway unit and integration tests

### 3. Intent-aware retrieval scaffolding

Implemented:
- retrieval profiles per `TaskIntent`
- domain-tag-aware query augmentation
- classifier domain tag extraction
- lighter retrieval for lightweight tasks and deeper retrieval for coding/analysis/planning style tasks

Main files:
- [crates/ngenorca-gateway/src/orchestration/classifier.rs](crates/ngenorca-gateway/src/orchestration/classifier.rs)
- [crates/ngenorca-memory/src/lib.rs](crates/ngenorca-memory/src/lib.rs)

Validated:
- unit tests covering domain tags and coding-oriented retrieval

### 4. Learned routing read path

Implemented:
- runtime consultation of learned routes before generic routing
- auto-accept path for learned routes when configured
- proper `from_memory` usage on learned routing decisions

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway unit tests

### 5. Session and runtime parity improvements

Implemented:
- anonymous sessions no longer implicitly reuse a shared session
- WebSocket supports explicit session reuse
- WebSocket and HTTP now share the logical `webchat` channel for parity
- runtime identity helpers now resolve canonical users for web and channel flows when identity bindings exist
- inbound channel worker now resolves bound channel identities before session/memory/orchestration handling

Main files:
- [crates/ngenorca-gateway/src/sessions.rs](crates/ngenorca-gateway/src/sessions.rs)
- [crates/ngenorca-gateway/src/runtime_identity.rs](crates/ngenorca-gateway/src/runtime_identity.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/src/lib.rs](crates/ngenorca-gateway/src/lib.rs)

Validated:
- gateway unit tests
- gateway integration tests

### 6. Tool execution context and telemetry

Implemented:
- tool execution now carries real session and user context
- tool execution events are published to the event bus
- sandbox-required tools are now blocked when sandboxing is disabled in config

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-gateway/src/plugins.rs](crates/ngenorca-gateway/src/plugins.rs)
- [crates/ngenorca-gateway/src/lib.rs](crates/ngenorca-gateway/src/lib.rs)
- [config/config.example.toml](config/config.example.toml)

Validated:
- gateway unit tests including sandbox enforcement coverage

### 7. Delegated worker behavior improvements

Implemented:
- added a worker delegation contract so sub-agents act explicitly on behalf of NgenOrca
- prevented worker prompts from behaving like separate top-level assistant identities
- added structured tool feedback payloads with `ok` and `retryable` fields
- blocked repeated identical tool calls in the same tool loop to reduce retry churn
- added corrective guidance after failed/blocked tool calls

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway unit tests for worker contract and tool feedback logic

### 8. Sandbox-backed command execution

Implemented:
- `run_command` now executes through the sandbox crate instead of directly spawning an unrestricted process from the gateway tool runtime
- sandbox execution now preserves the requested working directory
- command results now report sandbox status and detected sandbox environment
- workspace-oriented sandbox policy is applied to command execution

Main files:
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [crates/ngenorca-sandbox/src/lib.rs](crates/ngenorca-sandbox/src/lib.rs)

Validated:
- sandbox crate tests
- gateway tool tests for command output and working directory handling

### 9. Primary-agent synthesis over worker outputs

Implemented:
- when a non-primary worker produces the draft answer, the primary model now performs a synthesis pass before the final response is returned
- the synthesis pass reframes the answer explicitly as NgenOrca's final user-facing response
- synthesis instructions suppress internal routing/delegation leakage while preserving worker substance
- worker routing is still retained in orchestration metadata, but final response ownership is pulled back toward the primary assistant

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway unit tests for synthesis routing heuristics and synthesis prompt construction
- gateway test suite regression run

### 10. Workspace operating discipline and planning docs

Implemented:
- workspace-level instructions for parity auditing and complete-flow validation
- OpenClaw gap analysis document
- execution roadmap document

Main files:
- [.github/copilot-instructions.md](.github/copilot-instructions.md)
- [docs/OPENCLAW_GAP_ANALYSIS.md](docs/OPENCLAW_GAP_ANALYSIS.md)
- [docs/EXECUTION_ROADMAP.md](docs/EXECUTION_ROADMAP.md)

### 11. Tool verification pass and learned-route diagnostics

Implemented:
- added a post-tool verification pass so tool-backed drafts are checked against tool evidence before final synthesis
- tool loops now accumulate structured summaries of tool usage, failures, and blocked duplicate calls
- learned routing now records accept, escalation, and failure counters so repeated bad routes are penalized during lookup
- orchestration APIs now expose learned-route diagnostics, including effective confidence and penalty counts
- classification preview now consults learned routing so operator diagnostics match live routing behavior

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-gateway/src/orchestration/learned.rs](crates/ngenorca-gateway/src/orchestration/learned.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)

Validated:
- gateway unit tests for tool verification prompt construction, tool-loop summary merging, and learned-route penalty behavior
- gateway integration tests for orchestration diagnostics and learned-route preview behavior

### 12. Learned-routing operator controls

Implemented:
- added learned-routing policy config so operators can disable learned reuse, raise the effective-confidence threshold, and require a minimum number of observations before reuse
- orchestration diagnostics now expose the active learned-routing policy alongside the visible rule set
- added a dedicated learned-routes API for listing, filtering, and clearing learned rules without touching the backing database manually
- diagnostics APIs can optionally include penalized rules for investigation while keeping runtime reuse thresholds stricter by default

Main files:
- [crates/ngenorca-config/src/lib.rs](crates/ngenorca-config/src/lib.rs)
- [crates/ngenorca-gateway/src/orchestration/learned.rs](crates/ngenorca-gateway/src/orchestration/learned.rs)
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [config/config.example.toml](config/config.example.toml)
- [docs/CONFIGURATION_GUIDE.md](docs/CONFIGURATION_GUIDE.md)

Validated:
- gateway unit tests for learned-routing policy thresholds and operator control behavior
- gateway integration tests for learned-route listing, filtering, and clearing

### 13. Domain-specific verification and retry guidance

Implemented:
- tool-loop retries now include targeted guidance by tool domain instead of a single generic retry warning
- successful `write_file` calls now trigger a follow-up verification instruction so the model is pushed to read back or grep the modified file before finalizing
- tool verification prompts now include domain-specific checks for file reads/writes, command execution, and web retrieval so final answers stay aligned with actual tool evidence

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway unit tests for tool-loop verification state, domain-specific retry guidance, and verification prompt contents
- gateway crate regression run

### 14. Sandbox policy hardening and diagnostics

Implemented:
- added config-backed `[sandbox.policy]` controls so operators can explicitly govern network access, workspace writes, child-process spawning, extra read/write paths, and resource caps
- `run_command` now derives its effective sandbox policy from config instead of a hard-coded permissive runtime default
- command tool results now report the effective sandbox policy that was applied for that invocation
- health and status APIs now expose configured sandbox backend and policy details so operators can confirm the active runtime constraints
- aligned the example config and configuration guide with the actual implemented sandbox schema

Main files:
- [crates/ngenorca-config/src/lib.rs](crates/ngenorca-config/src/lib.rs)
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)
- [config/config.example.toml](config/config.example.toml)
- [docs/CONFIGURATION_GUIDE.md](docs/CONFIGURATION_GUIDE.md)

Validated:
- config crate tests for sandbox defaults and validation warnings
- gateway unit tests for command sandbox policy shaping and reporting
- gateway integration tests for sandbox diagnostics in health/status APIs
- config and gateway crate regression runs

### 15. Multi-worker handoff and synthesis reconciliation

Implemented:
- escalation now carries the previous worker draft and the quality-gate failure reason into the next worker call instead of restarting from a cold prompt
- augmentation now uses an explicit self-revision handoff so the worker is told exactly what gap must be filled while preserving still-correct sections
- primary synthesis now receives specialist draft history across sequential worker stages, allowing it to reconcile earlier and later drafts instead of seeing only the final worker output
- added unit coverage for escalation handoff, augmentation handoff, and multi-stage draft reconciliation

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway orchestrator unit tests for escalation handoff, augmentation handoff, and multi-stage synthesis messages
- gateway crate regression run

### 16. Structured tool verification and corrective remediation loop

Implemented:
- tool verification now requests structured JSON verdicts with groundedness, retry intent, concrete retry instructions, and the corrected user-facing answer
- when verification concludes that one more targeted tool pass would materially improve correctness, the primary layer now performs a single corrective remediation loop instead of stopping at a rewritten limitation
- remediation prompts carry forward verification issues, retry guidance, tool feedback, and pending write-readback state so the corrective pass is outcome-specific rather than generic
- after remediation, the response is verified again before final synthesis so the loop is now verify → correct → verify rather than a single post-tool rewrite

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway orchestrator unit tests for verification-report parsing, remediation prompt construction, and verified-response handling
- gateway crate regression run

### 17. First-class skills and automation artifacts

Implemented:
- added typed `SkillArtifact`, `AutomationStep`, and `SkillArtifactSummary` structures so reusable skills are explicit artifacts rather than loose prompt text
- added a persistent gateway `SkillStore` for saving and reloading structured skill artifacts from the runtime data directory
- added built-in `list_skills`, `read_skill`, and `save_skill` tools so the assistant can create and reuse automation recipes during normal tool use
- documented the new skill artifact tools in the README so operators can see that reusable skills are now part of the first-party capability set

Main files:
- [crates/ngenorca-plugin-sdk/src/lib.rs](crates/ngenorca-plugin-sdk/src/lib.rs)
- [crates/ngenorca-gateway/src/skills.rs](crates/ngenorca-gateway/src/skills.rs)
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [crates/ngenorca-gateway/src/lib.rs](crates/ngenorca-gateway/src/lib.rs)
- [README.md](README.md)

Validated:
- plugin SDK serde tests for skill artifacts
- gateway skill store tests and built-in tool roundtrip tests
- gateway crate regression run

### 18. Learned-route aging and stale diagnostics

Implemented:
- added learned-routing policy controls for route aging, maximum rule age, and per-day staleness penalties
- learned-route diagnostics now report age, staleness penalty, and stale status so operators can see when a previously good route has aged out
- runtime lookup now excludes stale learned rules while diagnostics APIs can still surface them for inspection with penalized output enabled
- orchestration diagnostics now summarize eligible vs stale vs penalized learned rules, and the learned-route endpoint can filter specifically for stale rules

Main files:
- [crates/ngenorca-config/src/lib.rs](crates/ngenorca-config/src/lib.rs)
- [crates/ngenorca-gateway/src/orchestration/learned.rs](crates/ngenorca-gateway/src/orchestration/learned.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)
- [config/config.example.toml](config/config.example.toml)
- [docs/CONFIGURATION_GUIDE.md](docs/CONFIGURATION_GUIDE.md)

Validated:
- config tests for learned-routing aging defaults and validation warnings
- learned-router unit tests for stale-rule filtering and age penalties
- gateway integration tests for stale learned-route reporting
- config and gateway crate regression runs

### 19. Device-signature and cross-channel identity edge cases

Implemented:
- wired device-signature-aware identity resolution into the live HTTP chat path, WebSocket chat path, and inbound channel worker path instead of leaving hardware-bound identity resolution isolated from serving
- block known-device signature failures in the live request loop so challenged device claims do not silently fall through to normal handling
- expanded runtime handle normalization so web handles, Telegram usernames, Matrix senders, and related channel identifiers can resolve across common prefix, casing, and alias variants
- added gateway regression coverage for valid web device signatures, invalid signed-device challenges, and richer Telegram/Matrix normalization cases

Main files:
- [crates/ngenorca-gateway/src/runtime_identity.rs](crates/ngenorca-gateway/src/runtime_identity.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/src/lib.rs](crates/ngenorca-gateway/src/lib.rs)
- [crates/ngenorca-gateway/Cargo.toml](crates/ngenorca-gateway/Cargo.toml)

Validated:
- gateway unit tests for device-aware runtime identity resolution and cross-channel normalization edge cases
- gateway crate regression run

### 20. Operator-facing worker and correction diagnostics

Implemented:
- extended orchestration responses with structured worker-stage diagnostics so live API responses now show initial worker handling plus escalation and augmentation outcomes
- exposed structured correction-loop diagnostics covering tool rounds, verification attempts, verification issues, and remediation success/failure state
- surfaced primary-synthesis attempt and fallback state in the same response metadata so operators can see when final ownership returned to the primary assistant
- expanded the orchestration info API to advertise the execution-diagnostics surfaces that are now available for live requests

Main files:
- [crates/ngenorca-plugin-sdk/src/lib.rs](crates/ngenorca-plugin-sdk/src/lib.rs)
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)

Validated:
- gateway orchestrator unit tests for execution-diagnostics shaping and merge behavior
- gateway integration tests for orchestration diagnostics exposure
- gateway crate regression run

### 21. Structured worker planning diagnostics and prompt scaffolding

Implemented:
- added structured delegation-plan diagnostics so complex delegated requests now surface a plan strategy and ordered execution steps in live orchestration metadata
- complex delegated tasks now build a plan before worker execution, choosing support agents for framing/verification when suitable and keeping the routed specialist focused on the main domain work
- worker, escalation, augmentation, and primary-synthesis prompt construction now receive the same structured plan so multi-stage delegated work shares one execution frame instead of independent prompt restarts
- expanded orchestration info diagnostics so operators can see that structured planning metadata is exposed at runtime

Main files:
- [crates/ngenorca-plugin-sdk/src/lib.rs](crates/ngenorca-plugin-sdk/src/lib.rs)
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)

Validated:
- gateway orchestrator unit tests for delegation-plan generation and prompt inclusion
- gateway integration tests for structured-planning diagnostics exposure
- plugin SDK serde tests for structured delegation-plan metadata
- gateway and plugin SDK regression runs

### 22. Parallel support-branch execution for complex delegated work

Implemented:
- structured plans can now launch a parallel support branch when a separate planning/analysis specialist should frame the work while the main routed worker executes the primary domain task
- the support branch runs concurrently with the main worker request, returns a bounded specialist draft for synthesis, and records operator-visible worker-stage diagnostics under `parallel-support`
- primary synthesis now receives both the support-branch draft and the main worker draft, allowing parallel framing/execution results to be reconciled into one final answer without exposing hidden delegation
- orchestration info now advertises the `parallel-support` stage alongside the existing initial/escalation/augmentation reporting

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)

Validated:
- gateway orchestrator unit tests for parallel-plan selection, support-step selection, and bounded support-branch prompts
- gateway integration tests for `parallel-support` diagnostics exposure
- gateway crate regression run

### 23. Skill lifecycle governance and review metadata

Implemented:
- extended skill artifacts with lifecycle metadata covering version, review status, operator-review requirements, review notes, and usage counters instead of treating stored skills as static JSON blobs
- `SkillStore` now preserves creation history, bumps versions on overwrite, records retrieval usage, and enforces review metadata for reviewed/approved skills so governance state survives normal tool use
- `list_skills` can now filter by lifecycle state, `read_skill` updates usage metadata, and `save_skill` accepts lifecycle fields for operator-reviewed automation recipes
- updated operator-facing documentation so built-in skill tooling now advertises lifecycle, review, and usage-tracking behavior

Main files:
- [crates/ngenorca-plugin-sdk/src/lib.rs](crates/ngenorca-plugin-sdk/src/lib.rs)
- [crates/ngenorca-gateway/src/skills.rs](crates/ngenorca-gateway/src/skills.rs)
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [README.md](README.md)

Validated:
- plugin SDK serde tests for lifecycle metadata and summary fields
- gateway skill store tests for version bumps, retrieval usage tracking, and governance validation
- gateway built-in tool tests for lifecycle-aware skill roundtrips and status filtering
- gateway and plugin SDK regression runs

### 24. Generalized multi-branch decomposition for delegated work

Implemented:
- generalized the earlier single `parallel-support` branch into multi-branch support execution so one delegated task can now launch multiple non-primary support workers alongside the main routed worker
- delegation planning can now add both framing and cross-check branches when suitable support agents are available, and the resulting plan strategy reflects whether the task stayed sequential, used one support branch, or used a broader parallel multi-branch split
- all support-branch results are now collected concurrently, added to specialist draft history, and surfaced through repeated `parallel-support` worker-stage diagnostics for operator visibility
- added orchestration coverage for both single-support and multi-branch delegation-plan construction paths

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway orchestrator unit tests for single-support vs multi-branch plan construction and support-step selection
- gateway crate regression run

### 25. Learned-route historical analytics summaries

Implemented:
- added learned-route history summaries grouped by intent, target agent, and domain so operators can see route trends instead of only inspecting individual learned rules
- orchestration diagnostics now expose historical learned-route summary buckets alongside the existing eligibility/staleness counts
- the learned-routes API now returns the same aggregated history view so operators can review routing drift and repeated escalation/failure patterns without manually aggregating rules
- added learned-router regression coverage for grouped history summaries and API-level assertions for the new history payloads

Main files:
- [crates/ngenorca-gateway/src/orchestration/learned.rs](crates/ngenorca-gateway/src/orchestration/learned.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)

Validated:
- learned-router unit tests for intent/agent/domain history aggregation
- gateway integration tests for learned-route history exposure
- gateway crate regression run

### 26. Executable automation review guards for skills

Implemented:
- added a reusable skill-validation report that classifies executable/risky tools, missing verification steps, operator-review requirements, and approval readiness for stored automation recipes
- executable skills now require explicit constraints plus per-step verification guidance before they can be saved, preventing loosely specified command/file automations from being persisted as reusable recipes
- added a built-in `validate_skill` tool and extended `save_skill` responses with validation output so operators and higher-level agents can review executable automation boundaries before approval or reuse
- updated the README so the built-in skill tooling now documents executable automation validation behavior directly

Main files:
- [crates/ngenorca-gateway/src/skills.rs](crates/ngenorca-gateway/src/skills.rs)
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [README.md](README.md)

Validated:
- gateway skill-store tests for executable automation validation and approval guardrails
- gateway built-in tool tests for `validate_skill` and validation-aware `save_skill` responses
- gateway crate regression run

### 27. Branch-specific verification and synthesis policy for multi-branch delegation

Implemented:
- specialist draft history now records branch role, reliability, and priority so synthesis can distinguish grounded execution work from advisory support branches
- tool-verified worker outputs are preserved as an explicit `verified` specialist stage, giving the primary synthesis pass a higher-confidence anchor after tool verification or remediation
- primary synthesis instructions now include branch reconciliation rules so grounded execution drafts outrank advisory support branches while still allowing support branches to contribute caveats and cross-checks

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)

Validated:
- gateway orchestrator unit tests for weighted specialist draft history and synthesis guidance
- gateway crate unit regression run
- gateway integration regression run

### 28. Adaptive learned-route decay and outcome trend shaping

Implemented:
- learned-route diagnostics now expose adaptive decay multipliers and recent outcome trend adjustments so operators can see why two rules with similar raw confidence are ranked differently
- effective confidence now incorporates recent accepted/escalated/failed outcomes with freshness-aware weighting instead of relying only on aggregate penalty counts plus a flat age penalty
- stale-route decay now adapts to recent failures and immature rules more aggressively while allowing recently accepted rules to retain slightly better reuse confidence

Main files:
- [crates/ngenorca-gateway/src/orchestration/learned.rs](crates/ngenorca-gateway/src/orchestration/learned.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)

Validated:
- learned-router unit tests for adaptive decay, outcome trend shaping, and diagnostics exposure
- gateway integration tests for learned-route diagnostics payloads
- gateway crate regression runs

### 29. Generated skill script previews and staged approval workflows

Implemented:
- skill validation now returns staged approval metadata so executable automation recipes can move through explicit `needs-fixes`, `awaiting-review`, `ready-for-approval`, and `approved` states based on review evidence
- executable skills now synthesize a bash script preview from structured steps and tool arguments, giving operators a concrete artifact to inspect before reuse or approval
- added a built-in `synthesize_skill_script` tool and extended existing skill validation/save flows so script previews and approval checklists are available without manual reconstruction

Main files:
- [crates/ngenorca-gateway/src/skills.rs](crates/ngenorca-gateway/src/skills.rs)
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [README.md](README.md)

Validated:
- gateway skill-store tests for script preview synthesis and approval stage reporting
- gateway built-in tool tests for `validate_skill`, `save_skill`, and `synthesize_skill_script`
- gateway crate regression runs

### 30. Runtime identity pairing and challenge diagnostics

Implemented:
- web `whoami`, HTTP chat, and WebSocket chat flows now expose structured runtime identity diagnostics including trust level, pairing/challenge state, suggested operator actions, and device or handle context
- device-verification failures no longer return only a generic error; they now include explicit challenge guidance so callers can retry with a fresh signed payload or re-pair rotated devices
- inbound channel-worker handling now logs structured pairing and challenge guidance, improving operator visibility when cross-channel identities are unpaired or degraded instead of silently behaving like generic low-context failures

Main files:
- [crates/ngenorca-gateway/src/runtime_identity.rs](crates/ngenorca-gateway/src/runtime_identity.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/src/lib.rs](crates/ngenorca-gateway/src/lib.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)
- [README.md](README.md)

Validated:
- gateway integration tests for `whoami` runtime identity guidance and structured chat challenge payloads
- gateway crate unit regression run
- gateway integration regression run

### 31. Interactive runtime identity pairing and challenge APIs

Implemented:
- added dedicated HTTP APIs to start and complete runtime identity pairing requests from web handles and device metadata instead of leaving pairing as diagnostics-only guidance
- added dedicated HTTP APIs to issue signed nonce challenges for known paired devices and verify challenge responses before restoring hardware trust
- pairing and challenge completion flows can now optionally rebind an existing session to the canonical user so live web conversations keep their continuity after identity recovery succeeds
- runtime identity diagnostics now advertise the dedicated pairing and challenge API paths so operator-facing flows can move directly from diagnosis to remediation

Main files:
- [crates/ngenorca-identity/src/lib.rs](crates/ngenorca-identity/src/lib.rs)
- [crates/ngenorca-identity/src/store.rs](crates/ngenorca-identity/src/store.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/src/sessions.rs](crates/ngenorca-gateway/src/sessions.rs)
- [crates/ngenorca-gateway/src/runtime_identity.rs](crates/ngenorca-gateway/src/runtime_identity.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)
- [README.md](README.md)

Validated:
- `cargo test -p ngenorca-identity --quiet`
- `cargo test -p ngenorca-gateway --quiet --lib`
- `cargo test -p ngenorca-gateway --quiet --test integration`
- identity crate: 20 tests passed
- gateway crate: 238 unit tests passed, 37 integration tests passed

### 32. Deeper operator history and trend diagnostics

Implemented:
- the orchestration diagnostics endpoint now includes a recent operator-history summary built from durable event-log replay, covering recent orchestration volume, latency, token usage, escalation/augmentation rates, learned-route reuse, and tool failure rates
- recent trend buckets now group the latest orchestration window by intent, target agent, quality outcome, and tool usage so operators can see repeated routing and failure patterns without manually inspecting raw events
- the event bus now supports recent-event replay directly, allowing trend aggregation to stay aligned across HTTP, WebSocket, and inbound worker publishing paths because all of them already emit durable orchestration and tool-execution events

Main files:
- [crates/ngenorca-bus/src/event_log.rs](crates/ngenorca-bus/src/event_log.rs)
- [crates/ngenorca-bus/src/lib.rs](crates/ngenorca-bus/src/lib.rs)
- [crates/ngenorca-gateway/src/routes.rs](crates/ngenorca-gateway/src/routes.rs)
- [crates/ngenorca-gateway/tests/integration.rs](crates/ngenorca-gateway/tests/integration.rs)
- [README.md](README.md)

Validated:
- bus crate tests for recent-event replay ordering
- gateway integration tests for orchestration recent-history summaries
- gateway crate unit and integration regression runs

### 33. Structured branch-specific memory slicing and evidence reconciliation for multi-branch delegation

Implemented:
- delegated execution branches now receive role-specific memory slices instead of the same raw context everywhere, so framing, cross-check, and execution workers each get a narrower semantic/episodic/working-memory view aligned to their branch purpose
- specialist draft history now records the memory scope, evidence focus, and concrete evidence slice seen by each branch, giving primary synthesis structured provenance instead of only stage ordering and prompt-level weighting
- live synthesis diagnostics now expose branch-evidence metadata and an explicit reconciliation strategy so operator-visible orchestration payloads can explain how memory slicing influenced delegated branch reconciliation

Main files:
- [crates/ngenorca-gateway/src/orchestration/orchestrator.rs](crates/ngenorca-gateway/src/orchestration/orchestrator.rs)
- [crates/ngenorca-plugin-sdk/src/lib.rs](crates/ngenorca-plugin-sdk/src/lib.rs)

Validated:
- plugin SDK serde tests for branch-evidence synthesis diagnostics
- gateway orchestrator unit tests for branch memory slicing and evidence-aware synthesis history
- gateway crate unit regression run
- gateway integration regression run

### 34. Direct staged skill execution, journaling, and rollback application

Implemented:
- added a built-in `execute_skill_stages` tool that can execute generated skill-preview steps directly for supported `run_command` and `write_file` actions instead of limiting executable skills to validation and script-preview output only
- staged execution now requires rollback guidance and explicit checkpoints for mutating preview steps, persists execution journals during the run, and records lifecycle metadata so stored skills keep an auditable execution trail
- supported write steps now prepare rollback artifacts ahead of time and can automatically apply rollback entries after downstream failures, turning staged skill execution into an actual guarded executor instead of a read-only rehearsal flow
- failed command launches are now recorded as failed staged-step results instead of aborting the journal path early, which keeps rollback application and failure diagnostics consistent when a command cannot even be spawned

Main files:
- [crates/ngenorca-gateway/src/tools.rs](crates/ngenorca-gateway/src/tools.rs)
- [crates/ngenorca-gateway/src/skills.rs](crates/ngenorca-gateway/src/skills.rs)
- [crates/ngenorca-plugin-sdk/src/lib.rs](crates/ngenorca-plugin-sdk/src/lib.rs)

Validated:
- `cargo test -p ngenorca-gateway --quiet --lib`
- `cargo test -p ngenorca-gateway --quiet --test integration`
- `cargo test -p ngenorca-plugin-sdk --quiet`
- `cargo test --workspace --quiet`
- gateway crate: 252 unit tests passed, 41 integration tests passed
- plugin SDK crate: 20 tests passed
- full workspace regression passed

## Current validated state

Most recent direct validation:
- `cargo test -p ngenorca-memory`
- `cargo test -p ngenorca-gateway`

Recent gateway result after the latest delegation/tool-runtime changes:
- 191 unit tests passed
- 28 integration tests passed

Most recent validation after sandbox-backed command execution changes:
- `cargo test -p ngenorca-sandbox`
- `cargo test -p ngenorca-gateway`
- sandbox crate: 11 tests passed
- gateway crate: 191 unit tests passed, 28 integration tests passed

Most recent validation after tool verification and learned-route diagnostics changes:
- `cargo test -p ngenorca-gateway`
- gateway crate: 196 unit tests passed, 30 integration tests passed

Most recent validation after learned-routing operator controls changes:
- `cargo test -p ngenorca-gateway`
- gateway crate: 197 unit tests passed, 31 integration tests passed

Most recent validation after domain-specific verification and retry guidance changes:
- `cargo test -p ngenorca-gateway`
- gateway crate: 199 unit tests passed, 31 integration tests passed

Most recent validation after sandbox policy hardening and diagnostics changes:
- `cargo test -p ngenorca-config`
- `cargo test -p ngenorca-gateway`
- config crate: 48 tests passed
- gateway crate: 200 unit tests passed, 32 integration tests passed

Most recent validation after multi-worker handoff and synthesis reconciliation changes:
- `cargo test -p ngenorca-gateway orchestrator`
- `cargo test -p ngenorca-gateway`
- gateway orchestrator slice: 22 tests passed
- gateway crate: 203 unit tests passed, 32 integration tests passed

Most recent validation after structured tool verification and corrective remediation loop changes:
- `cargo test -p ngenorca-gateway orchestrator`
- `cargo test -p ngenorca-gateway`
- gateway orchestrator slice: 25 tests passed
- gateway crate: 206 unit tests passed, 32 integration tests passed

Most recent validation after first-class skills and automation artifact changes:
- `cargo test -p ngenorca-plugin-sdk`
- `cargo test -p ngenorca-gateway`
- plugin SDK crate: 18 tests passed
- gateway crate: 213 unit tests passed, 32 integration tests passed

Most recent validation after learned-route aging and stale diagnostics changes:
- `cargo test -p ngenorca-config`
- `cargo test -p ngenorca-gateway`
- config crate: 49 tests passed
- gateway crate: 215 unit tests passed, 33 integration tests passed

Most recent validation after device-signature and identity edge-case changes:
- `cargo test -p ngenorca-gateway`
- gateway crate: 219 unit tests passed, 33 integration tests passed

Most recent validation after operator-facing worker and correction diagnostics changes:
- `cargo test -p ngenorca-gateway`
- `cargo test -p ngenorca-plugin-sdk`
- gateway crate: 220 unit tests passed, 33 integration tests passed
- plugin SDK crate: 18 tests passed

Most recent validation after structured worker planning changes:
- `cargo test -p ngenorca-gateway`
- `cargo test -p ngenorca-plugin-sdk`
- gateway crate: 222 unit tests passed, 33 integration tests passed
- plugin SDK crate: 19 tests passed

Most recent validation after parallel support-branch execution changes:
- `cargo test -p ngenorca-gateway`
- gateway crate: 224 unit tests passed, 33 integration tests passed

Most recent validation after skill lifecycle governance changes:
- `cargo test -p ngenorca-plugin-sdk`
- `cargo test -p ngenorca-gateway`
- plugin SDK crate: 20 tests passed
- gateway crate: 226 unit tests passed, 33 integration tests passed

Most recent validation after generalized multi-branch decomposition changes:
- `cargo test -p ngenorca-gateway`
- gateway crate: 227 unit tests passed, 33 integration tests passed

Most recent validation after learned-route historical analytics changes:
- `cargo test -p ngenorca-gateway`
- `cargo test -p ngenorca-gateway --test integration`
- gateway crate: 228 unit tests passed, 33 integration tests passed

Most recent validation after executable automation review guard changes:
- `cargo test -p ngenorca-gateway`
- gateway crate: 231 unit tests passed, 33 integration tests passed

Most recent validation after branch-specific synthesis policy changes:
- `cargo test -p ngenorca-gateway --lib`
- `cargo test -p ngenorca-gateway --test integration`
- gateway crate: 231 unit tests passed, 33 integration tests passed

Most recent validation after adaptive learned-route decay changes:
- `cargo test -p ngenorca-gateway --lib`
- `cargo test -p ngenorca-gateway --test integration`
- gateway crate: 233 unit tests passed, 33 integration tests passed

Most recent validation after generated skill script workflow changes:
- `cargo test -p ngenorca-gateway --lib`
- `cargo test -p ngenorca-gateway --test integration`
- gateway crate: 235 unit tests passed, 33 integration tests passed

Most recent validation after runtime identity diagnostics changes:
- `cargo test -p ngenorca-gateway --lib`
- `cargo test -p ngenorca-gateway --test integration`
- gateway crate: 235 unit tests passed, 35 integration tests passed

Most recent validation after operator history trend diagnostics changes:
- `cargo test -p ngenorca-bus --quiet`
- `cargo test -p ngenorca-gateway --lib`
- `cargo test -p ngenorca-gateway --test integration`
- bus crate: 17 tests passed
- gateway crate: 235 unit tests passed, 35 integration tests passed

Most recent validation after branch-specific memory slicing and evidence reconciliation changes:
- `cargo test -p ngenorca-plugin-sdk --quiet`
- `cargo test -p ngenorca-gateway --quiet --lib`
- `cargo test -p ngenorca-gateway --quiet --test integration`
- plugin SDK crate: 20 tests passed
- gateway crate: 235 unit tests passed, 35 integration tests passed

Most recent validation after correction-trace, contradiction-scoring, and richer skill/diagnostic metadata changes:
- `cargo test -p ngenorca-plugin-sdk --quiet`
- `cargo test -p ngenorca-core --quiet`
- `cargo test -p ngenorca-gateway --quiet --lib`
- `cargo test -p ngenorca-gateway --quiet --test integration`
- plugin SDK crate: 20 tests passed
- core crate: 0 direct tests in target completed successfully
- gateway crate: 237 unit tests passed, 35 integration tests passed

Most recent validation after interactive runtime identity API changes:
- `cargo test -p ngenorca-identity --quiet`
- `cargo test -p ngenorca-gateway --quiet --lib`
- `cargo test -p ngenorca-gateway --quiet --test integration`
- identity crate: 20 tests passed
- gateway crate: 238 unit tests passed, 37 integration tests passed

Most recent validation after direct staged skill execution and rollback changes:
- `cargo test -p ngenorca-gateway --quiet --lib`
- `cargo test -p ngenorca-gateway --quiet --test integration`
- `cargo test -p ngenorca-plugin-sdk --quiet`
- `cargo test --workspace --quiet`
- gateway crate: 252 unit tests passed, 41 integration tests passed
- plugin SDK crate: 20 tests passed
- full workspace regression passed

## Pending risks and gaps

These are the main risks still considered open.

### High priority

1. **Self-correction loops are still shallow**
   - the system now blocks repeated identical failing tool calls, classifies tool failures more explicitly, records per-attempt correction traces, verifies tool-grounded drafts, can perform one targeted corrective remediation pass, and runs a final post-synthesis verification pass so synthesized answers cannot silently drift away from verified tool evidence
   - but it is not yet a broader multi-step planner with repeated recovery strategies or deeper automated remediation policies across longer operational chains

2. **Unified runtime identity is substantially improved but still has follow-up gaps**
   - channel/web binding resolution and device-signature-based identity now participate in live serving paths, live web APIs expose structured pairing/challenge diagnostics, and dedicated pairing/challenge endpoints now let operators actually complete remediation and rebind live sessions
   - but richer challenge UX, stronger automated linking workflows, and broader channel-specific normalization remain open follow-up work

3. **Sandbox coverage is improved but still not maximal**
   - command execution now goes through config-driven sandbox policy controls and operator diagnostics expose the active backend and policy
   - filesystem/network/process restrictions still depend on platform backend behavior and can be hardened further, especially around backend-specific enforcement guarantees and richer policy auditing

### Medium priority

4. **Primary-agent synthesis is much stronger but still not a full contradiction-resolution engine for large multi-worker runs**
   - delegated work now includes branch-specific memory slices, structured branch-evidence diagnostics, and a contradiction scan that scores conflicting branches and injects that summary into the primary synthesis pass
   - but the system still relies on heuristic conflict scoring plus prompt instructions rather than a richer contradiction graph or tool-backed evidence scoring model across many simultaneous branches

5. **Learned routing is active but still simple**
   - successful routes now influence runtime decisions, repeated escalations/failures reduce reuse confidence, stale rules age out with explicit diagnostics, operators can inspect learned-route history by intent/agent/domain, recent outcomes shape effective confidence more explicitly, and grounded/recovered accepts now feed richer outcome classes into learned-rule trend scoring
   - but learned routing still lacks richer decay windows, stronger long-horizon trend modeling, and more structured exploration/exploitation controls

6. **Cross-channel identity normalization may still miss edge cases**
   - web, Telegram, Matrix, and device-backed cases are stronger now, but some channels may still need richer metadata or provider-specific alias handling for robust canonical identity linking

7. **Tool result verification and remediation are still incomplete**
   - the verification pass now includes domain-aware checks for file edits, command outcomes, and web retrieval claims, and can trigger one corrective remediation pass
   - but it still lacks richer verification for multi-step edit/build workflows and deeper correction policies across repeated operational failures

### Lower priority but still important

8. **Skills/automation artifacts are now present but still early**
   - NgenOrca now has typed reusable skill artifacts plus built-in tools to validate, save, retrieve, synthesize script previews, and directly execute supported staged steps with persisted journals, checkpoints, rollback planning, and rollback application for supported writes
   - but skill execution still covers only a narrow directly executable tool set, with broader lifecycle analytics, resumed checkpoint flows, and richer planner/executor behavior still left as follow-up work

9. **Operator-facing diagnostics are improved, but deeper history is still open**
   - live responses and orchestration info now expose worker-stage, correction-loop, synthesis outcomes, contradiction summaries, per-attempt correction traces, richer tool failure classes, and recent event-log trend summaries including per-user/per-channel mixes across recent requests
   - but the system still lacks longer-horizon persistence policies, dedicated filtered history endpoints, and dedicated timelines for correction behavior over longer operational windows

## Recommended next implementation order

1. longer-horizon per-user and per-channel operator trend slices
2. stronger contradiction scoring across multi-branch evidence for large delegated runs
3. stronger per-platform sandbox policy tightening and audit guarantees
4. richer automated cross-channel identity normalization and challenge UX
5. broader direct skill-execution coverage, resumed checkpoints, and lifecycle analytics

## Fix now vs follow-up improvement

### Fix now
- no immediate parity blocker from the staged-skill execution tranche remains after the latest regression fix

### Follow-up improvement
- stronger contradiction scoring across multi-branch evidence for large delegated runs
- dedicated interactive pairing and challenge-response APIs plus richer cross-channel identity normalization
- longer-horizon per-user/per-channel diagnostics and trend aggregation
- broader direct skill-execution coverage, resumed checkpoints, and lifecycle analytics
- stronger per-platform sandbox policy tightening

## Update rule for this file

When another parity tranche is completed, append or update:
- what changed
- which files/flows were affected
- what was validated
- what residual risk remains

This file should stay focused on current operational status, not just plans.