# NgenOrca vs OpenClaw Gap Analysis

This document records the current comparison between NgenOrca and OpenClaw so the repository has a shared source of truth for architectural gaps, missing operational behavior, and the work required to close the gap.

## Purpose

NgenOrca already has a credible architecture skeleton:

- primary assistant identity
- multi-provider LLM support
- three-tier memory
- built-in tools
- routing and sub-agent profiles
- quality gating and escalation
- event bus, sessions, and channels

The main problem is not the absence of architecture. The main problem is that several critical capabilities are only partially operational, or are implemented on one path but not all equivalent paths.

OpenClaw feels more autonomous because its runtime integrates:

- strong identity/persona framing
- memory that is actively used in the serving loop
- typed tools that remain available to the agent
- persistent automation primitives
- explicit session/agent coordination
- operational self-correction loops

## Comparison Summary

## Areas where NgenOrca already has the architecture

### Assistant identity

NgenOrca now has a stronger primary assistant prompt in the orchestrator, preserving the identity of NgenOrca rather than acting like a raw model passthrough.

### Memory

NgenOrca has real three-tier memory:

- Tier 1: working memory
- Tier 2: episodic memory
- Tier 3: semantic memory

The manager exists and retrieval/consolidation paths exist.

### Tools

NgenOrca has first-party tools for:

- listing directories
- reading and writing files
- searching the workspace
- fetching URLs
- web search
- running commands

It also has a tool-call loop in the orchestrator.

### Orchestration and sub-agent routing

NgenOrca has:

- intent classification
- routing strategies
- sub-agent profiles
- escalation/augmentation paths
- learned routing storage

## Areas where NgenOrca is still behind OpenClaw

### 1. Memory is not yet a decision engine

Current behavior is still too generic.

What is missing:

- retrieval should be driven by intent and domain, not just current query text
- memory should affect planning and delegation strategy
- user-specific relevant memory should be injected consistently across all surfaces
- semantic facts should be promoted and reused more reliably

Example target behavior:

- `Planning` and `Analysis` should retrieve deeper prior context
- `Coding` should prioritize technical preferences, repo context, and prior solutions
- `QuestionAnswering` should use lightweight retrieval
- `Conversation` should preserve continuity and preferences
- `ToolUse` should retrieve recent operational context and relevant prior actions

### 2. Learned routing is not yet operationally in charge

Today the system records orchestration outcomes, but runtime decisions are not materially improved by those learned results.

Target behavior:

- learned successful routes should be consulted before generic routing
- learned failure patterns should reduce confidence or prevent bad routing
- domain tags and user context should influence route reuse

### 3. Sub-agents are profiles, not a real cooperating network

Current sub-agents mostly behave as model-selection profiles.

Target behavior:

- the primary assistant remains the owner of identity, memory, and capability framing
- worker agents receive narrowly scoped tasks with the exact context required
- worker agents return structured outputs for synthesis by the primary layer
- worker agents should not impersonate the top-level assistant
- multi-step decomposition should be possible without losing coherence

### 4. Tools exist, but not yet inside a strong autonomy loop

Current tooling is useful but incomplete for OpenClaw-like autonomy.

What is still missing:

- stronger sandbox enforcement
- preserved session/user/tool execution context
- better eventing/audit for tool execution
- tool-result verification and retry logic
- script/automation generation as a first-class workflow
- reusable skill/task recipes

### 5. Runtime parity is still a structural risk

Equivalent paths still need to be treated as first-class siblings.

Target behavior:

- HTTP, WebSocket, and background/channel-worker flows should behave consistently
- session reuse rules should match
- memory writes and reads should match
- orchestration context should match
- tool availability and telemetry should match

## Priority Gap List

The most important blockers to OpenClaw-like behavior are:

1. memory is not yet used as the main decision context
2. learned routing is mostly write-only
3. sub-agents are not yet coordinated workers
4. tool execution is not yet fully contextualized and verified
5. flow parity across HTTP, WebSocket, and worker paths is incomplete
6. unified runtime identity is not fully in the serving loop

## Design Principles for Closing the Gap

To close the gap, NgenOrca should evolve according to these principles:

- the primary assistant keeps ownership of identity and continuity
- memory retrieval is intent-aware and user-aware
- sub-agents are delegated workers, not alternate personalities
- tool use includes verification and correction, not just invocation
- learned behavior is read back into future decisions
- all equivalent runtime paths are kept in parity

## Definition of Success

NgenOrca should be considered meaningfully closer to OpenClaw when:

- the same user gets coherent behavior across HTTP, WebSocket, and channels
- the assistant consistently knows who it is and what it can do
- memory is used based on task intent and user context
- successful past routing materially affects future routing
- worker agents can help the primary agent without breaking identity
- tools are used, checked, and retried with context
- scripts, automations, and reusable skills are first-class agent workflows

## Relationship to the roadmap

The concrete execution plan is tracked separately in [docs/EXECUTION_ROADMAP.md](EXECUTION_ROADMAP.md).
