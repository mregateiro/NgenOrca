# NgenOrca Configuration Guide

This guide walks you through every section of NgenOrca's configuration system — from connecting your first LLM to wiring up messaging channels, tuning memory, sandboxing, identity, and observability.

---

## Table of Contents

1. [How Config Works](#how-config-works)
2. [Connecting LLMs](#connecting-llms)
   - [Anthropic (Claude)](#anthropic-claude)
   - [OpenAI (GPT)](#openai-gpt)
   - [Ollama (Local Models)](#ollama-local-models)
   - [Azure OpenAI](#azure-openai)
   - [Google Gemini](#google-gemini)
   - [OpenRouter (Multi-Provider)](#openrouter-multi-provider)
   - [Custom / Self-Hosted](#custom--self-hosted)
3. [Connecting Channels](#connecting-channels)
   - [WebChat (Built-in)](#webchat-built-in)
   - [WhatsApp](#whatsapp)
   - [Telegram](#telegram)
   - [Discord](#discord)
   - [Slack](#slack)
   - [Signal](#signal)
   - [Matrix](#matrix)
   - [Microsoft Teams](#microsoft-teams)
4. [Gateway Settings](#gateway-settings)
5. [Identity & Security](#identity--security)
6. [Memory System](#memory-system)
7. [Sandbox Settings](#sandbox-settings)
8. [Observability](#observability)
9. [Environment Variables](#environment-variables)
10. [Full Example Config](#full-example-config)
11. [Docker-Specific Config](#docker-specific-config)
12. [Multi-User Setup](#multi-user-setup)
13. [Troubleshooting](#troubleshooting)

---

## How Config Works

NgenOrca uses a **layered config system** — later sources override earlier ones:

```
1. Built-in defaults          (always present, sensible for local use)
2. System config              (/etc/ngenorca/ or %PROGRAMDATA%\ngenorca\)
3. User config                (~/.ngenorca/config.toml)
4. Plugin configs             (~/.ngenorca/plugins/<name>/config.toml)
5. Environment variables      (NGENORCA_* — see "Environment Variables" section)
6. CLI flags                  (--config, --port, etc.)
```

**Config file location:**

| Platform | Path |
|----------|------|
| Windows  | `C:\Users\<you>\.ngenorca\config.toml` |
| macOS    | `/Users/<you>/.ngenorca/config.toml` |
| Linux    | `/home/<you>/.ngenorca/config.toml` |

You can also pass a custom path:
```bash
ngenorca gateway --config /path/to/my-config.toml
```

The file format is [TOML](https://toml.io). If no config file exists, NgenOrca runs with safe defaults (localhost-only, no external connections).

---

## Connecting LLMs

The `[agent]` section controls which LLM NgenOrca talks to. The `model` field uses a **provider/model** naming convention.

### Anthropic (Claude)

```toml
[agent]
model = "anthropic/claude-sonnet-4-20250514"
# thinking_level = "Medium"        # Off | Minimal | Low | Medium | High | Max
# workspace = "~/.ngenorca/workspace"

[agent.providers.anthropic]
api_key = "sk-ant-api03-..." 
# base_url = "https://api.anthropic.com"    # default
# max_tokens = 8192
# temperature = 0.7
```

Available Anthropic models:
| Model ID | Description | Context |
|----------|-------------|---------|
| `anthropic/claude-opus-4-20250514` | Most capable, deep reasoning | 200K |
| `anthropic/claude-sonnet-4-20250514` | Balanced speed/capability | 200K |
| `anthropic/claude-haiku-3-20250307` | Fastest, cheapest | 200K |

> **Where to get your key:** [console.anthropic.com](https://console.anthropic.com) → API Keys

---

### OpenAI (GPT)

```toml
[agent]
model = "openai/gpt-4o"

[agent.providers.openai]
api_key = "sk-proj-..."
# base_url = "https://api.openai.com/v1"   # default
# organization = "org-..."                  # optional
# max_tokens = 4096
# temperature = 0.7
```

Available OpenAI models:
| Model ID | Description | Context |
|----------|-------------|---------|
| `openai/gpt-4o` | Flagship multimodal | 128K |
| `openai/gpt-4o-mini` | Smaller, cheaper | 128K |
| `openai/o3` | Reasoning model | 200K |
| `openai/o4-mini` | Fast reasoning | 200K |

> **Where to get your key:** [platform.openai.com/api-keys](https://platform.openai.com/api-keys)

---

### Ollama (Local Models)

Run models entirely on your own hardware — no API key needed, fully private.

**Step 1:** Install Ollama from [ollama.com](https://ollama.com) and pull a model:
```bash
ollama pull llama3.1
ollama pull deepseek-r1:8b
ollama pull qwen2.5:14b
```

**Step 2:** Configure NgenOrca to use it:
```toml
[agent]
model = "ollama/llama3.1"

[agent.providers.ollama]
base_url = "http://127.0.0.1:11434"        # Ollama's default address
# keep_alive = "5m"                         # How long to keep model in VRAM
# num_ctx = 8192                            # Context window size (affects VRAM)
```

Popular local models:
| Model ID | VRAM Needed | Good For |
|----------|-------------|----------|
| `ollama/llama3.1` | ~8 GB | General assistant |
| `ollama/llama3.1:70b` | ~40 GB | High-quality reasoning |
| `ollama/deepseek-r1:8b` | ~6 GB | Code + reasoning |
| `ollama/qwen2.5:14b` | ~10 GB | Multilingual |
| `ollama/phi-4` | ~8 GB | Compact & fast |
| `ollama/mistral` | ~5 GB | Lightweight general use |

> **Tip:** If you have less than 8 GB VRAM, use quantized models (e.g., `llama3.1:8b-q4_0`). NgenOrca's memory system works with the KV-cache optimizer described in the architecture docs, so even small context windows work efficiently.

---

### Azure OpenAI

For enterprise deployments using Azure-hosted OpenAI models:

```toml
[agent]
model = "azure/my-gpt4o-deployment"

[agent.providers.azure]
api_key = "your-azure-key"
endpoint = "https://your-resource.openai.azure.com"
api_version = "2024-10-21"
deployment = "my-gpt4o-deployment"          # Your deployment name in Azure portal
```

> **Where to get your key:** Azure Portal → your OpenAI resource → Keys and Endpoint

---

### Google Gemini

```toml
[agent]
model = "google/gemini-2.0-flash"

[agent.providers.google]
api_key = "AIza..."
# base_url = "https://generativelanguage.googleapis.com/v1beta"   # default
```

Available models:
| Model ID | Description | Context |
|----------|-------------|---------|
| `google/gemini-2.5-pro` | Most capable | 1M |
| `google/gemini-2.0-flash` | Fast and efficient | 1M |

> **Where to get your key:** [aistudio.google.com/apikey](https://aistudio.google.com/apikey)

---

### OpenRouter (Multi-Provider)

OpenRouter gives you access to many models through a single API key:

```toml
[agent]
model = "openrouter/anthropic/claude-sonnet-4"

[agent.providers.openrouter]
api_key = "sk-or-v1-..."
# base_url = "https://openrouter.ai/api/v1"    # default
# site_name = "NgenOrca"                        # Shows in OpenRouter dashboard
# fallback_models = [                           # Auto-failover
#     "openrouter/openai/gpt-4o",
#     "openrouter/google/gemini-2.0-flash",
# ]
```

> **Where to get your key:** [openrouter.ai/keys](https://openrouter.ai/keys)
>
> **Why use this?** One API key for 200+ models, automatic failover, unified billing.

---

### Custom / Self-Hosted

For any OpenAI-compatible API (vLLM, LM Studio, text-generation-webui, LocalAI, etc.):

```toml
[agent]
model = "custom/my-finetuned-model"

[agent.providers.custom]
base_url = "http://192.168.1.100:8080/v1"  # Your server's address
api_key = "not-needed"                      # Some servers require a dummy key
# model_name = "my-finetuned-model"         # Model name the server expects
```

**LM Studio** example:
```toml
[agent]
model = "custom/lmstudio"

[agent.providers.custom]
base_url = "http://127.0.0.1:1234/v1"
api_key = "lm-studio"
```

**vLLM** example:
```toml
[agent]
model = "custom/Qwen/Qwen2.5-72B-Instruct"

[agent.providers.custom]
base_url = "http://gpu-server:8000/v1"
api_key = "token-abc123"
```

---

## Connecting Channels

Channels are messaging surfaces that NgenOrca can listen on. Each channel is a **plugin** — you enable it in config, provide credentials, and NgenOrca handles the rest.

### WebChat (Built-in)

WebChat is built into the gateway — no extra setup needed. It's available at `http://localhost:18789` when the gateway starts.

```toml
[channels.webchat]
enabled = true                                # enabled by default
# theme = "dark"                              # dark | light | auto
# max_upload_size_mb = 10
```

This is the easiest way to start talking to NgenOrca.

---

### WhatsApp

NgenOrca connects to WhatsApp via the WhatsApp Business Cloud API.

```toml
[channels.whatsapp]
enabled = true
phone_number_id = "1234567890"                # From Meta developer dashboard
access_token = "EAAx..."                      # Permanent token (not the temp one)
verify_token = "my-secret-verify-token"       # You make this up, use it in webhook setup
webhook_path = "/webhooks/whatsapp"           # NgenOrca listens here for incoming msgs
app_secret = "abc123..."                      # For verifying webhook signatures
```

**Setup steps:**
1. Go to [developers.facebook.com](https://developers.facebook.com) → Create App → Business type
2. Add the WhatsApp product
3. In WhatsApp → API Setup, note your **Phone Number ID** and generate a **permanent token**
4. Set your webhook URL to `https://your-domain.com/webhooks/whatsapp`
5. Use the `verify_token` you chose above when configuring the webhook
6. Subscribe to `messages` webhook field

> **Important:** WhatsApp requires HTTPS with a valid certificate. Use a reverse proxy (nginx/Caddy) or a tunnel (Cloudflare Tunnel, ngrok) in front of NgenOrca.

---

### Telegram

```toml
[channels.telegram]
enabled = true
bot_token = "7123456789:AAH..."               # From @BotFather
# webhook_url = "https://your-domain.com/webhooks/telegram"    # Optional: use webhook mode
# polling = true                              # true = long polling (no HTTPS needed), false = webhook
# allowed_users = [123456789, 987654321]      # Telegram user IDs to allow (empty = all)
```

**Setup steps:**
1. Message [@BotFather](https://t.me/BotFather) on Telegram
2. Send `/newbot` and follow the prompts
3. Copy the bot token into your config

> **Tip:** Use `polling = true` for local development (no public URL needed). Switch to webhook mode for production.

---

### Discord

```toml
[channels.discord]
enabled = true
bot_token = "MTIz..."                         # From Discord Developer Portal
# application_id = "123456789"
# guild_ids = ["111111111", "222222222"]       # Restrict to specific servers (empty = all)
# allowed_roles = ["Admin", "NgenOrca-User"]   # Restrict by role name
# command_prefix = "!"                         # For text commands (e.g., !ask)
```

**Setup steps:**
1. Go to [discord.com/developers/applications](https://discord.com/developers/applications)
2. Create New Application → Bot → Reset Token → copy it
3. Under OAuth2 → URL Generator: select `bot` scope + `Send Messages`, `Read Message History` permissions
4. Use the generated URL to invite the bot to your server

---

### Slack

```toml
[channels.slack]
enabled = true
bot_token = "xoxb-..."                        # Bot User OAuth Token
app_token = "xapp-..."                        # App-Level Token (for Socket Mode)
signing_secret = "abc123..."                  # For webhook signature verification
# socket_mode = true                          # true = Socket Mode (no public URL needed)
# channel_ids = ["C01234567"]                 # Restrict to specific channels
```

**Setup steps:**
1. Go to [api.slack.com/apps](https://api.slack.com/apps) → Create New App
2. Enable **Socket Mode** (easier) or set up a webhook URL
3. Under OAuth & Permissions, add scopes: `chat:write`, `app_mentions:read`, `im:history`, `im:read`
4. Install to workspace and copy the Bot Token
5. For Socket Mode, generate an App-Level Token under Basic Information

---

### Signal

NgenOrca connects to Signal via [signal-cli](https://github.com/AsamK/signal-cli) or [signald](https://signald.org).

```toml
[channels.signal]
enabled = true
phone_number = "+1234567890"                  # Your Signal number
# signal_cli_path = "/usr/local/bin/signal-cli"
# data_path = "~/.local/share/signal-cli"
# mode = "daemon"                             # daemon | dbus
```

**Setup steps:**
1. Install signal-cli: `brew install signal-cli` (macOS) or download from GitHub
2. Register: `signal-cli -u +1234567890 register`
3. Verify: `signal-cli -u +1234567890 verify CODE`
4. Start in daemon mode: `signal-cli -u +1234567890 daemon --json`

---

### Matrix

```toml
[channels.matrix]
enabled = true
homeserver = "https://matrix.org"             # Or your self-hosted Synapse/Dendrite
user_id = "@ngenorca:matrix.org"
access_token = "syt_..."
# device_id = "NGENORCA01"
# auto_join = true                            # Auto-join rooms when invited
# encrypted = true                            # Enable E2EE (requires libolm)
```

**Setup steps:**
1. Register a Matrix account for the bot
2. Get an access token: `curl -X POST https://matrix.org/_matrix/client/r0/login -d '{"type":"m.login.password","user":"ngenorca","password":"***"}'`
3. Copy the `access_token` from the response

---

### Microsoft Teams

```toml
[channels.teams]
enabled = true
app_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
app_password = "your-bot-secret"
tenant_id = "your-tenant-id"                  # "common" for multi-tenant
# webhook_url = "https://your-domain.com/webhooks/teams"
```

**Setup steps:**
1. Go to [dev.botframework.com](https://dev.botframework.com) → Create Bot
2. Register a new bot and note the App ID and Password
3. In Azure Portal → Bot Channels Registration → configure the Teams channel
4. Set the messaging endpoint to your NgenOrca webhook URL

---

## Gateway Settings

The gateway is NgenOrca's front door — it serves the HTTP API, WebSocket connections, and routes messages.

```toml
[gateway]
bind = "127.0.0.1"             # Listen address ("0.0.0.0" to expose to network)
port = 18789                   # Port number
auth_mode = "None"             # None | Password | Token | Certificate

# Password mode:
# auth_password = "my-secret"

# Token mode:
# auth_tokens = ["token-abc-123", "token-def-456"]

# Certificate mode (mTLS):
# tls_cert = "/path/to/cert.pem"
# tls_key = "/path/to/key.pem"
# tls_ca = "/path/to/ca.pem"          # Client CA for mTLS verification
```

Auth mode recommendations:
| Scenario | Auth Mode | Why |
|----------|-----------|-----|
| Local-only, single user | `None` | Simplest, already firewalled |
| Exposed to LAN | `Token` | Easy to configure, secure enough |
| Internet-facing | `Certificate` | Strongest — mutual TLS |

---

## Identity & Security

Controls how NgenOrca knows **who** is talking.

```toml
[identity]
require_hardware_attestation = true     # Require TPM/Secure Enclave for owner
biometrics_enabled = false              # Voice print, typing cadence (future)
auto_lock_minutes = 30                  # Lock after N minutes idle

# Trust level thresholds — what can each trust level do?
# Hardware (TPM)    → Full access: memory, tools, admin commands
# Certificate       → Full access except identity management
# Channel           → Standard access: chat, basic tools
# Unknown           → Read-only or blocked
```

### First-Time Setup (Onboarding)

When NgenOrca starts for the first time, the `onboard` command walks you through:

```bash
ngenorca onboard
```

This will:
1. Generate a hardware-bound keypair (TPM if available, software fallback otherwise)
2. Create the owner user identity
3. Set up the initial config file
4. Optionally link your first channel

### Multi-Device Pairing

To use NgenOrca from a new device:
```bash
ngenorca identity pair
```
This generates a pairing code on the new device. Approve it from an already-paired device.

---

## Memory System

NgenOrca has three memory tiers that work together to give the agent persistent, long-term context.

```toml
[memory]
enabled = true                          # Master switch for the memory system

# --- Working Memory (in-RAM per session) ---
# No config needed — this is the current conversation window.

# --- Episodic Memory (SQLite) ---
episodic_max_entries = 100000           # Max entries before oldest are pruned
# episodic_db = "~/.ngenorca/data/episodic.db"    # Custom DB path

# --- Semantic Memory (distilled facts) ---
semantic_token_budget = 4096            # Max tokens injected into each prompt
consolidation_interval_secs = 3600     # How often to distill episodic → semantic (seconds)
# semantic_min_confidence = 0.5         # Ignore facts below this confidence
# semantic_decay_rate = 0.01            # How fast unused facts lose confidence
```

### How the Tiers Work

```
┌─────────────────────────────────────────────┐
│  Working Memory       (this conversation)   │
│  - Current messages, tool results, context  │
│  - Evicted when context window is full      │
└──────────────────┬──────────────────────────┘
                   │ overflow
                   ▼
┌─────────────────────────────────────────────┐
│  Episodic Memory      (everything stored)   │
│  - Full conversation logs per session       │
│  - Searchable by embeddings (future)        │
│  - Pruned after episodic_max_entries        │
└──────────────────┬──────────────────────────┘
                   │ consolidation (every N seconds)
                   ▼
┌─────────────────────────────────────────────┐
│  Semantic Memory      (distilled knowledge) │
│  - "User prefers dark mode"                 │
│  - "Miguel's timezone is EST"              │
│  - Facts with confidence scores & decay     │
│  - Injected into every prompt               │
└─────────────────────────────────────────────┘
```

### Tuning Tips

| Scenario | Setting |
|----------|---------|
| Low RAM device | `episodic_max_entries = 10000` |
| Privacy-first | `enabled = false` (no long-term memory) |
| Power user | `semantic_token_budget = 8192` (more context per prompt) |
| Fast consolidation | `consolidation_interval_secs = 600` (every 10 min) |

---

## Sandbox Settings

Controls how NgenOrca isolates tool execution (commands, code, file access).

```toml
[sandbox]
enabled = true                # Master switch — disable to run tools unsandboxed
backend = "Auto"              # How to sandbox tool execution
```

Backend options:

| Value | Platform | What It Does |
|-------|----------|-------------|
| `Auto` | Any | Picks best option for your platform (recommended) |
| `WindowsJob` | Windows | Uses Windows Job Objects + restricted tokens |
| `LinuxSeccomp` | Linux | Uses seccomp-bpf + landlock + namespaces |
| `MacOsSandbox` | macOS | Uses macOS App Sandbox |
| `Container` | Docker | Defers to container's own isolation |
| `None` | Any | **No sandboxing** — tools run with full privileges |

> **Warning:** Setting `backend = "None"` means tool calls (file writes, shell commands, etc.) run with your full user permissions. Only do this on a trusted, isolated system.

### Fine-Grained Sandbox Policy (Advanced)

```toml
[sandbox.policy]
# allow_network = false                # Whether tools can make network calls
# allowed_paths = [                    # Directories tools can read/write
#     "~/.ngenorca/workspace",
#     "/tmp/ngenorca-scratch",
# ]
# memory_limit_mb = 512                # Max memory per tool invocation
# cpu_limit_seconds = 30               # Max CPU time per tool invocation
# wall_time_limit_seconds = 60         # Max wall clock time
```

---

## Observability

Monitor NgenOrca's behavior and performance.

```toml
[observability]
log_level = "info"            # trace | debug | info | warn | error
json_logs = false             # true = JSON-structured logs (for log aggregators)
otlp_enabled = false          # Enable OpenTelemetry export
otlp_endpoint = "http://localhost:4317"   # OTLP gRPC endpoint
```

### Log Levels

| Level | When To Use |
|-------|-------------|
| `error` | Production — only problems |
| `warn` | Production — problems + warnings |
| `info` | Default — startup, connections, high-level flow |
| `debug` | Development — includes message routing details |
| `trace` | Troubleshooting — extremely verbose, includes raw payloads |

### Connecting to Grafana / Jaeger

Enable OTLP and point to your collector:

```toml
[observability]
otlp_enabled = true
otlp_endpoint = "http://jaeger:4317"    # Or your Grafana Alloy/OTEL collector
log_level = "debug"
json_logs = true
```

---

## Environment Variables

Every config field can be set via environment variables. The pattern is:

```
NGENORCA_<SECTION>__<FIELD>=value
```

Note the **double underscore** (`__`) between sections.

Examples:

```powershell
# Windows PowerShell
$env:NGENORCA_GATEWAY__PORT = "9999"
$env:NGENORCA_GATEWAY__BIND = "0.0.0.0"
$env:NGENORCA_AGENT__MODEL = "openai/gpt-4o"
$env:NGENORCA_OBSERVABILITY__LOG_LEVEL = "debug"

# Secrets — never put these in config files in production!
$env:NGENORCA_AGENT__PROVIDERS__OPENAI__API_KEY = "sk-proj-..."
$env:NGENORCA_CHANNELS__TELEGRAM__BOT_TOKEN = "7123456789:AAH..."
```

```bash
# Linux / macOS
export NGENORCA_GATEWAY__PORT=9999
export NGENORCA_AGENT__MODEL="ollama/llama3.1"
export NGENORCA_AGENT__PROVIDERS__ANTHROPIC__API_KEY="sk-ant-..."
```

> **Best practice:** Put secrets (API keys, tokens) in environment variables, not in the config file. The config file should contain only non-sensitive settings.

---

## Full Example Config

Here's a complete, real-world config for someone using Claude as the main model, Ollama as a local fallback, with Telegram and Discord channels:

```toml
# ~/.ngenorca/config.toml

# ---------- DATA ----------
data_dir = "~/.ngenorca/data"

# ---------- GATEWAY ----------
[gateway]
bind = "0.0.0.0"
port = 18789
auth_mode = "Token"
# Set via: NGENORCA_GATEWAY__AUTH_TOKENS='["my-secret-token"]'

# ---------- AGENT / LLM ----------
[agent]
model = "anthropic/claude-sonnet-4-20250514"
thinking_level = "Medium"
workspace = "~/.ngenorca/workspace"

[agent.providers.anthropic]
# Set via env: NGENORCA_AGENT__PROVIDERS__ANTHROPIC__API_KEY
max_tokens = 8192
temperature = 0.7

[agent.providers.ollama]
base_url = "http://127.0.0.1:11434"
# Used as fallback when Anthropic API is down or for privacy-sensitive tasks

# ---------- CHANNELS ----------
[channels.webchat]
enabled = true

[channels.telegram]
enabled = true
polling = true
# Set via env: NGENORCA_CHANNELS__TELEGRAM__BOT_TOKEN
allowed_users = [123456789]

[channels.discord]
enabled = true
# Set via env: NGENORCA_CHANNELS__DISCORD__BOT_TOKEN
guild_ids = ["111111111111111111"]

# ---------- IDENTITY ----------
[identity]
require_hardware_attestation = true
auto_lock_minutes = 60

# ---------- MEMORY ----------
[memory]
enabled = true
episodic_max_entries = 50000
semantic_token_budget = 4096
consolidation_interval_secs = 1800

# ---------- SANDBOX ----------
[sandbox]
enabled = true
backend = "Auto"

# ---------- OBSERVABILITY ----------
[observability]
log_level = "info"
json_logs = false
otlp_enabled = false
```

---

## Docker-Specific Config

When running in Docker, use environment variables for all secrets and mount the config file:

```yaml
# docker-compose.yml
services:
  ngenorca:
    image: ngenorca:latest
    ports:
      - "18789:18789"
    volumes:
      - ./config.toml:/root/.ngenorca/config.toml:ro
      - ngenorca-data:/root/.ngenorca/data
    environment:
      # Secrets go here (or use Docker secrets / .env file)
      NGENORCA_AGENT__PROVIDERS__ANTHROPIC__API_KEY: "${ANTHROPIC_API_KEY}"
      NGENORCA_CHANNELS__TELEGRAM__BOT_TOKEN: "${TELEGRAM_BOT_TOKEN}"
      NGENORCA_CHANNELS__DISCORD__BOT_TOKEN: "${DISCORD_BOT_TOKEN}"
      # Override settings per-environment
      NGENORCA_GATEWAY__BIND: "0.0.0.0"
      NGENORCA_OBSERVABILITY__LOG_LEVEL: "info"
      NGENORCA_OBSERVABILITY__JSON_LOGS: "true"

volumes:
  ngenorca-data:
```

Create a `.env` file (git-ignored!) for your secrets:
```env
ANTHROPIC_API_KEY=sk-ant-api03-...
TELEGRAM_BOT_TOKEN=7123456789:AAH...
DISCORD_BOT_TOKEN=MTIz...
```

> **Sandbox note:** Inside Docker, the sandbox backend auto-detects `Container` mode and defers isolation to Docker's own cgroups/namespaces. No extra sandbox config needed.

---

## Multi-User Setup

NgenOrca supports multiple users (e.g., a household). Each user gets isolated memory profiles.

```toml
# The owner is set up during `ngenorca onboard`.
# Additional users are added via the identity system:

[identity]
require_hardware_attestation = true     # For the owner
# Guest users are auto-created at Channel trust level
# when they message via a connected channel.
```

### Adding a Family Member

```bash
# 1. Create the user
ngenorca identity create --name "Partner" --role household

# 2. They pair their phone
ngenorca identity pair --user partner-user-id

# 3. Link their Telegram handle
ngenorca identity link --user partner-user-id --channel telegram --handle "@partner"
```

### User Roles

| Role | Can Do | Memory |
|------|--------|--------|
| `Owner` | Everything — admin, config, identity management | Full access to all memory |
| `Household` | Chat, use tools, manage own sessions | Isolated memory profile |
| `Guest` | Chat only, no tools, no persistent memory | Session-only (no storage) |

---

## Troubleshooting

### "Connection refused" to LLM

```bash
# Check if the provider is reachable
curl https://api.anthropic.com/v1/messages -H "x-api-key: YOUR_KEY" -H "content-type: application/json"

# For Ollama, make sure it's running
ollama list
curl http://localhost:11434/api/tags
```

### "Channel not receiving messages"

1. Check the channel is `enabled = true` in config
2. Check logs: `ngenorca gateway --log-level debug`
3. For webhook channels (WhatsApp, Teams): verify your public URL is reachable
4. For polling channels (Telegram): verify the bot token is correct

### "Identity verification failed"

```bash
# Check your device's identity status
ngenorca identity list
ngenorca doctor
```

### Config Not Loading

```bash
# Validate your config file
ngenorca doctor

# Check which config sources are active
NGENORCA_OBSERVABILITY__LOG_LEVEL=debug ngenorca gateway
# Look for "Loading config file" in the output

# Check for TOML syntax errors
ngenorca info --config /path/to/config.toml
```

### Check System Health

```bash
ngenorca doctor
```

This runs diagnostics on:
- Config file syntax
- LLM provider connectivity
- Channel adapter status
- Database integrity
- Sandbox capabilities
- Hardware identity status
