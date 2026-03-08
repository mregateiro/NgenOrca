# 🐋 NgenOrca — Personal AI Assistant

**Microkernel architecture · Hardware-bound identity · Three-tier memory**

NgenOrca is a personal AI assistant you run on your own devices. Built in Rust for minimal footprint (~15-20MB binary, ~10-20MB RAM), it runs natively on Windows, Linux, and macOS — or inside Docker with zero changes.

## Why NgenOrca?

| | Traditional Assistants | NgenOrca |
|---|---|---|
| **Runtime** | Node.js / Python (150-300MB RAM) | Rust (10-20MB RAM) |
| **Architecture** | Monolith | Microkernel + plugins |
| **Message durability** | Ephemeral | SQLite WAL event log |
| **Security** | Opt-in sandboxing | Sandboxed by default |
| **Plugins** | Prompt-based | Typed SDK with permissions |
| **Identity** | Channel handle | Hardware-bound (TPM/Secure Enclave) |
| **Memory** | Per-session, lost on restart | Three-tier: working → episodic → semantic |
| **Local models** | Not supported | First-class (Ollama, llama.cpp) |
| **Observability** | Basic logging | OpenTelemetry built-in |

## LLM Providers

Connect to any major provider — or run fully local with Ollama:

| Provider | Config Key | Local? |
|----------|-----------|--------|
| **Anthropic** (Claude) | `anthropic/claude-sonnet-4-20250514` | No |
| **OpenAI** (GPT) | `openai/gpt-4o` | No |
| **Ollama** (local) | `ollama/llama3.1` | ✅ Yes |
| **Azure OpenAI** | `azure/my-deployment` | No |
| **Google Gemini** | `google/gemini-2.0-flash` | No |
| **OpenRouter** | `openrouter/anthropic/claude-sonnet-4` | No |
| **Custom** (vLLM, LM Studio) | `custom/my-model` | ✅ Yes |

## Channels

Talk to NgenOrca from anywhere — all channels resolve to one unified identity:

| Channel | Mode | Public URL needed? |
|---------|------|-------------------|
| **WebChat** | Built-in | No |
| **Telegram** | Polling or Webhook | No (polling) |
| **Discord** | Gateway | No |
| **WhatsApp** | Webhook | Yes (HTTPS) |
| **Slack** | Socket Mode or Webhook | No (socket mode) |
| **Signal** | signal-cli daemon | No |
| **Matrix** | Sync | No |
| **Teams** | Webhook | Yes |

## Quick Start

### Native (recommended)

```bash
git clone https://github.com/ngenorca/ngenorca.git
cd ngenorca
cargo build --release
./target/release/ngenorca gateway
```

### Docker

```bash
cp .env.example .env           # Fill in your API keys
docker compose up -d
```

See [docs/DOCKER.md](docs/DOCKER.md) for the full step-by-step guide (config, Ollama, auth, troubleshooting).
Using Portainer? See [docs/PORTAINER.md](docs/PORTAINER.md) for the web-UI walkthrough.

### NAS / Homelab (with Authelia + nginx)

```bash
cp .env.example .env           # Fill in your API keys
# Copy deploy/nginx/ngenorca.conf to your nginx config
docker compose -f docker-compose.nas.yml up -d
```

See [docs/NAS_DEPLOYMENT.md](docs/NAS_DEPLOYMENT.md) for the full guide.

## Authentication

| Mode | Use Case |
|------|----------|
| `None` | Local-only, loopback |
| `TrustedProxy` | Behind nginx + Authelia/Authentik (NAS/homelab) |
| `Token` | API access with bearer tokens |
| `Password` | Basic auth for simple setups |
| `Certificate` | mTLS for high-security |

The **TrustedProxy** mode reads identity from reverse proxy headers — Authelia authenticates the user (including 2FA), nginx passes `Remote-User` / `Remote-Email` / `Remote-Groups` headers to NgenOrca.

## Architecture

```
Messaging channels → Gateway (control plane) → Agent → Response
        │                    │
        │                    ├─ Event Bus (SQLite WAL — durable)
        │                    ├─ Identity Manager (TPM/SE/StrongBox)
        │                    ├─ Memory Manager (3-tier)
        │                    ├─ Plugin Manager (typed SDK)
        │                    ├─ Auth Middleware (TrustedProxy/Token/mTLS)
        │                    └─ Sandbox (OS-adaptive)
        │
        ├─ WhatsApp     (plugin)
        ├─ Telegram     (plugin)
        ├─ Discord      (plugin)
        ├─ Slack        (plugin)
        ├─ WebChat      (built-in)
        └─ ...
```

### Crate Map

| Crate | Purpose |
|---|---|
| `ngenorca-core` | Shared types, traits, error definitions |
| `ngenorca-bus` | Durable event bus (SQLite WAL + broadcast) |
| `ngenorca-config` | Composable config — providers, channels, auth, proxy headers |
| `ngenorca-identity` | Hardware-bound identity & cross-channel unification |
| `ngenorca-memory` | Three-tier memory (working/episodic/semantic) |
| `ngenorca-plugin-sdk` | Typed plugin SDK with lifecycle hooks |
| `ngenorca-sandbox` | Platform-adaptive sandboxing |
| `ngenorca-gateway` | HTTP/WebSocket gateway with auth middleware |
| `ngenorca-cli` | CLI binary (`ngenorca` command) |

## Three-Tier Memory

```
┌─────────────────────────────────────────┐
│  Tier 1: Working Memory (hot)           │
│  Active conversation context window     │
│  KV-cache persistence for local models  │
└────────────────┬────────────────────────┘
                 │ overflow / session end
                 ▼
┌─────────────────────────────────────────┐
│  Tier 2: Episodic Memory (warm)         │
│  Full conversation logs + embeddings    │
│  Semantic search (RAG over history)     │
└────────────────┬────────────────────────┘
                 │ background consolidation
                 ▼
┌─────────────────────────────────────────┐
│  Tier 3: Semantic Memory (persistent)   │
│  Distilled facts, preferences, people   │
│  Temporal decay, contradiction resolve  │
└─────────────────────────────────────────┘
```

## Configuration

Minimal config:
```toml
[agent]
model = "anthropic/claude-sonnet-4-20250514"
```

NAS with Authelia:
```toml
[gateway]
auth_mode = "TrustedProxy"

[agent]
model = "anthropic/claude-sonnet-4-20250514"

[channels.telegram]
enabled = true
polling = true
```

Full reference: [`config/config.example.toml`](config/config.example.toml)

Guides:
- [Configuration Guide](docs/CONFIGURATION_GUIDE.md) — all providers, channels, and settings
- [NAS Deployment](docs/NAS_DEPLOYMENT.md) — WireGuard + nginx + Authelia setup

## API Endpoints

| Endpoint | Description |
|---|---|
| `GET /health` | Health check (no auth) |
| `GET /metrics` | Prometheus-compatible metrics |
| `GET /api/v1/status` | System status + caller identity |
| `GET /api/v1/whoami` | Show authenticated user (verify Authelia flow) |
| `GET /api/v1/providers` | Configured LLM providers |
| `GET /api/v1/channels` | Configured channel adapters |
| `GET /api/v1/identity/users` | Registered users |
| `GET /api/v1/memory/stats` | Memory system statistics |
| `GET /api/v1/events/count` | Event log count |
| `POST /api/v1/chat` | Send a chat message |
| `WS /ws` | WebSocket (chat + real‑time event push) |

## CLI Commands

```
ngenorca gateway          Start the gateway server
ngenorca status           Show running gateway status
ngenorca onboard          Interactive first-time setup
ngenorca identity list    List registered users
ngenorca identity pair    Pair a new device
ngenorca identity revoke  Revoke a device
ngenorca doctor           Diagnose common issues
ngenorca info             Show system information
```

## Deployment Modes

| Mode | Best For | Config |
|------|----------|--------|
| Native Windows | Desktop use | `ngenorca gateway` |
| Native Linux/macOS | Server/daemon | systemd/launchd |
| Docker standalone | Quick deploy | `docker compose up` |
| Docker + nginx + Authelia | **NAS / homelab** | `docker-compose.nas.yml` |
| Split containers | Production | Each adapter isolated |

## Project Structure

```
NgenOrca/
├── crates/
│   ├── ngenorca-core/          # Shared types & traits
│   ├── ngenorca-bus/           # Durable event bus
│   ├── ngenorca-config/        # Config with providers & channels
│   ├── ngenorca-identity/      # Hardware-bound identity
│   ├── ngenorca-memory/        # Three-tier memory
│   ├── ngenorca-plugin-sdk/    # Plugin SDK
│   ├── ngenorca-sandbox/       # Platform-adaptive sandbox
│   ├── ngenorca-gateway/       # HTTP/WS gateway + auth middleware
│   └── ngenorca-cli/           # CLI binary
├── config/
│   └── config.example.toml     # Full config reference
├── deploy/
│   └── nginx/
│       └── ngenorca.conf       # nginx + Authelia site config
├── docs/
│   ├── CONFIGURATION_GUIDE.md  # LLM, channel, and settings guide
│   └── NAS_DEPLOYMENT.md       # NAS/homelab deployment guide
├── docker-compose.yml          # Standard Docker deploy
├── docker-compose.nas.yml      # NAS deploy (Authelia + nginx)
├── .env.example                # Secrets template
├── Dockerfile                  # Multi-stage Alpine build
└── README.md
```

## Development

```bash
cargo build                      # Build all crates
cargo check --workspace          # Check compilation
cargo test --workspace           # Run tests (~320 tests)
cargo run -p ngenorca-cli -- gateway --verbose
```

### CI/CD

GitHub Actions workflows in `.github/workflows/`:

- **ci.yml** — Format, clippy, test (Linux/macOS/Windows), build, Docker build
- **release.yml** — Cross-compile binaries, multi-arch Docker image, GitHub Release

### Operational Features

| Feature | Description |
|---|---|
| **Rate Limiting** | Per-user sliding window (configurable `rate_limit_max` / `rate_limit_window_secs`) |
| **Metrics** | Prometheus-compatible endpoint at `/metrics` (HTTP, WS, orchestration, token counters) |
| **Retry with Backoff** | Transient provider errors (429, 500, 503, timeout) auto-retry up to 3× with exponential backoff |
| **Config Validation** | Pre-flight checks for ports, auth credentials, model names, thresholds, duplicate sub-agents |
| **Graceful Shutdown** | Ctrl+C / SIGTERM → drain in-flight requests → shutdown plugins → clean exit |
| **Ed25519 Device Signing** | Hardware-bound identity verification with `ring` Ed25519 signatures |

## Community

- [CHANGELOG.md](CHANGELOG.md) — Version history and release notes
- [CONTRIBUTING.md](CONTRIBUTING.md) — How to contribute
- [SECURITY.md](SECURITY.md) — Security policy and vulnerability reporting

## License

MIT
