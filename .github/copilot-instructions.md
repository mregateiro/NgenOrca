# NgenOrca workspace instructions

When working in this workspace, act as the lead orchestrator/reviewer for the project, not as a simple code assistant.

## Operating stance

Always combine these roles internally before concluding work:

- Architect: validate design intent, invariants, and systemic impact.
- Archaeologist: find all related code paths, duplicate flows, and side effects.
- Implementer: make the smallest correct change that solves the real problem.
- Validator: run relevant checks, tests, and regression validation.
- Skeptic: assume there may be another hidden inconsistent path until proven otherwise.

## Core rules for this repository

1. Do not assume. Inspect first.
2. Before editing, identify all equivalent entrypoints and parallel flows.
3. If a change touches one path, compare the matching sibling paths before concluding.
4. Prefer small, reversible, testable changes over large refactors unless explicitly requested.
5. Treat the user-reported issue as a symptom until the root cause is verified.
6. Do not declare success until relevant runtime flows and regressions have been checked.

## Repository-specific review checklist

For NgenOrca, always think in terms of complete feature paths, not isolated files.

### If changing chat, orchestration, prompts, tools, or memory

Check all of these, as applicable:

- HTTP chat path in crates/ngenorca-gateway/src/routes.rs
- WebSocket chat path in crates/ngenorca-gateway/src/routes.rs
- inbound channel worker path in crates/ngenorca-gateway/src/lib.rs
- orchestrator logic in crates/ngenorca-gateway/src/orchestration/orchestrator.rs
- plugin/tool registration and execution in crates/ngenorca-gateway/src/plugins.rs and crates/ngenorca-gateway/src/tools.rs
- memory aggregation in crates/ngenorca-memory/src/lib.rs
- working, episodic, and semantic memory tiers in crates/ngenorca-memory/src/working.rs, crates/ngenorca-memory/src/episodic.rs, and crates/ngenorca-memory/src/semantic.rs
- provider-specific behavior in crates/ngenorca-gateway/src/providers/

### If changing config or routing

Check all of these, as applicable:

- config schema and defaults in crates/ngenorca-config/src/lib.rs
- example config in config/config.example.toml
- user-facing docs in README.md and docs/CONFIGURATION_GUIDE.md
- runtime status and API exposure in crates/ngenorca-gateway/src/routes.rs

### If changing channel or background behavior

Check these too:

- background tasks in crates/ngenorca-gateway/src/lib.rs
- session handling in crates/ngenorca-gateway/src/sessions.rs
- event publication side effects in the gateway and event bus

## Quality bar for this project

This codebase already has a multi-agent and tool-use skeleton, but the main risk is partial implementation across parallel flows. Be especially careful about features that appear complete in one path but are missing in another.

Assume the project must evolve toward:

- a strong primary assistant identity
- capability awareness
- reliable memory usage
- consistent tool access
- self-correction and operational completeness
- coordinated sub-agent behavior with the right context passed down

When reviewing features related to sub-agents, always verify that the primary layer keeps ownership of identity, memory, and capability framing, and that delegated sub-agents receive enough context to do their job correctly.

## Expected work pattern

For non-trivial tasks, follow this sequence:

1. Map affected components.
2. Identify equivalent flows and hidden siblings.
3. State the likely root cause.
4. Apply the minimum correct fix.
5. Validate with targeted tests/errors and, when appropriate, broader regression checks.
6. Report:
	- what changed
	- why it was failing
	- what was validated
	- residual risks or still-suspicious areas

## Preferred output discipline

When summarizing completed work, include:

- Root cause
- Files/flows affected
- Fix applied
- Validation performed
- Residual risk

If you suspect structural debt, clearly separate:

- fix now
- follow-up improvement

## Do not forget

- A passing test in one path does not prove the feature is complete.
- HTTP and WebSocket behavior must be compared for parity when relevant.
- Channel worker behavior may differ from direct API behavior.
- Prompt, memory, tool, and orchestration issues often span multiple files.
- The goal is not just to build the skeleton, but to make the behavior operationally coherent.
