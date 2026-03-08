# Changelog

All notable changes to NgenOrca will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2025-06-01

### Added

- **Microkernel gateway** — axum 0.8 HTTP + WebSocket server with modular crate architecture.
- **9 workspace crates**: `ngenorca-core`, `ngenorca-bus`, `ngenorca-config`, `ngenorca-identity`, `ngenorca-memory`, `ngenorca-plugin-sdk`, `ngenorca-sandbox`, `ngenorca-gateway`, `ngenorca-cli`.
- **Hardware-bound identity** — Ed25519 device fingerprinting with TPM/Secure Enclave detection, owner pairing flow via CLI.
- **Three-tier memory** — working (in-process), episodic (SQLite FTS5), semantic (SQLite with confidence scoring). Background consolidation every 5 minutes.
- **Multi-provider LLM support** — Anthropic Claude, OpenAI-compatible, and local Ollama with automatic retry and exponential backoff.
- **SLM cascade classifier** — keyword-based task classification (coding, translation, summarization, analysis) with local-first routing.
- **Orchestration pipeline** — classifier → router → provider → quality gate → optional escalation. Learned routing rules improve over time.
- **8 channel adapters** — WebChat (built-in WebSocket), Telegram (full polling/webhook), Discord, Slack, WhatsApp, Signal, Matrix, Microsoft Teams (stubs with Plugin + ChannelAdapter traits).
- **Plugin system** — `PluginRegistry` with manifest-based registration, tool execution, health checks, and graceful shutdown.
- **Sandbox enforcement** — configurable `SandboxPolicy` with allowed paths, blocked commands, timeout, and environment detection (Docker/WSL/native).
- **Event bus** — `EventBus` with typed publish/subscribe, per-session/per-type filtering, SQLite-backed event log with replay and pruning (7-day retention).
- **Session management** — `SessionManager` with per-user/per-channel isolation, automatic TTL (2 hours), and background cleanup every 5 minutes.
- **Rate limiting** — token-bucket middleware with configurable max requests, time window, and per-key tracking. Metrics integration.
- **Metrics** — Prometheus-compatible counters (requests, errors, rate-limited, provider calls, consolidations, latency histogram).
- **Authentication** — None, Bearer token, Basic, and TrustedProxy (Authelia/Authentik header forwarding) modes.
- **CLI** — `ngenorca pair`, `ngenorca status`, and `ngenorca run` commands with interactive device pairing.
- **Docker support** — multi-stage Alpine build, docker-compose with health checks, resource limits, and NAS deployment guide.
- **Configuration** — TOML-based config with validation, serde defaults, and comprehensive `config.example.toml`.

### Security

- Owner identity is hardware-bound via Ed25519 keypair derived from device fingerprint.
- Sandbox blocks arbitrary filesystem and process access by default.
- Rate limiting prevents abuse on all API endpoints.
- TLS configuration documented for production deployments.

[Unreleased]: https://github.com/ngenorca/ngenorca/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ngenorca/ngenorca/releases/tag/v0.1.0
