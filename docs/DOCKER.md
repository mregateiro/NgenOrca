# Docker Quick Start Guide

Run NgenOrca in Docker with a single command. This guide covers both local-only setups (Ollama) and cloud LLM providers (Anthropic, OpenAI).

---

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) ≥ 24.0
- [Docker Compose](https://docs.docker.com/compose/install/) ≥ 2.20 (usually bundled with Docker Desktop)
- An LLM provider API key **or** [Ollama](https://ollama.com) running on the host

## Step 1: Clone the Repository

```bash
git clone https://github.com/ngenorca/ngenorca.git
cd ngenorca
```

## Step 2: Create Your Configuration

### Option A: Cloud Provider (Anthropic, OpenAI, etc.)

Create a `.env` file in the project root:

```bash
cat > .env << 'EOF'
# ── LLM Provider ──
# Pick ONE provider and set its key. Leave the others blank.
ANTHROPIC_API_KEY=sk-ant-api03-your-key-here
OPENAI_API_KEY=
NGENORCA_MODEL=anthropic/claude-sonnet-4-20250514

# ── Optional: Telegram bot ──
TELEGRAM_ENABLED=false
TELEGRAM_BOT_TOKEN=

# ── Optional: Discord bot ──
DISCORD_ENABLED=false
DISCORD_BOT_TOKEN=
EOF
```

Then uncomment the provider env vars in `docker-compose.yml`:

```yaml
environment:
  - NGENORCA_AGENT__MODEL=${NGENORCA_MODEL:-anthropic/claude-sonnet-4-20250514}
  - NGENORCA_AGENT__PROVIDERS__ANTHROPIC__API_KEY=${ANTHROPIC_API_KEY}
```

### Option B: Fully Local with Ollama (No API Keys)

1. Install and start [Ollama](https://ollama.com) on your host machine.
2. Pull a model: `ollama pull llama3.1`
3. Create your `.env`:

```bash
cat > .env << 'EOF'
NGENORCA_MODEL=ollama/llama3.1
OLLAMA_URL=http://host.docker.internal:11434
EOF
```

4. Add the Ollama env vars to `docker-compose.yml`:

```yaml
environment:
  - NGENORCA_AGENT__MODEL=${NGENORCA_MODEL:-ollama/llama3.1}
  - NGENORCA_AGENT__PROVIDERS__OLLAMA__BASE_URL=${OLLAMA_URL:-http://host.docker.internal:11434}
```

> **Note:** `host.docker.internal` resolves to the host machine on Docker Desktop (macOS/Windows). On Linux, add `--add-host=host.docker.internal:host-gateway` or use the host's LAN IP.

### Option C: Custom Config File

For advanced configuration, edit the TOML file:

```bash
cp config/config.example.toml config/config.toml
nano config/config.toml
```

The config directory is mounted read-only into the container at `/etc/ngenorca`.

## Step 3: Start NgenOrca

```bash
docker compose up -d
```

This will:
1. Build the binary from source (first run takes 2–5 minutes)
2. Create a minimal Alpine container (~25MB)
3. Start the gateway on port `18789`

Watch the logs:
```bash
docker logs -f ngenorca
```

You should see:
```
NgenOrca gateway listening on 0.0.0.0:18789
  Auth:      None
  Provider:  anthropic/claude-sonnet-4-20250514
  Health:    http://0.0.0.0:18789/health
  Chat:      POST http://0.0.0.0:18789/api/v1/chat
  WebSocket: ws://0.0.0.0:18789/ws
```

## Step 4: Verify It's Running

```bash
# Health check
curl http://localhost:18789/health

# System status
curl http://localhost:18789/api/v1/status

# Configured providers
curl http://localhost:18789/api/v1/providers
```

## Step 5: Send Your First Message

```bash
curl -s -X POST http://localhost:18789/api/v1/chat \
  -H "Content-Type: application/json" \
  -d '{"message": "Hello! What can you do?", "channel": "webchat"}' | jq .
```

Or connect via WebSocket:
```bash
# Using websocat (install: cargo install websocat)
websocat ws://localhost:18789/ws
# Then type JSON: {"message": "Hello!"}
```

## Step 6: Use the Pre-Built Image (Optional)

Instead of building from source, use the published image from GitHub Container Registry:

```yaml
# In docker-compose.yml, replace:
#   build: .
# With:
    image: ghcr.io/ngenorca/ngenorca:latest
```

Then:
```bash
docker compose pull
docker compose up -d
```

Multi-arch images are available for `linux/amd64` and `linux/arm64`.

---

## Common Operations

### Stop
```bash
docker compose down
```

### Update
```bash
git pull
docker compose up -d --build
```

### View Metrics (Prometheus)
```bash
curl http://localhost:18789/metrics
```

### Check Active Sessions
```bash
curl http://localhost:18789/api/v1/sessions
```

### View Event Count
```bash
curl http://localhost:18789/api/v1/events/count
```

---

## Data Persistence

NgenOrca stores all data in a Docker volume:

| Volume | Container Path | Contents |
|--------|----------------|----------|
| `ngenorca-data` | `/var/lib/ngenorca` | SQLite databases (events, identity, memory) |
| `./config` | `/etc/ngenorca` (read-only) | Configuration files |

To back up your data:
```bash
docker run --rm -v ngenorca-data:/data -v $(pwd):/backup alpine \
  tar czf /backup/ngenorca-backup-$(date +%Y%m%d).tar.gz /data
```

To restore:
```bash
docker run --rm -v ngenorca-data:/data -v $(pwd):/backup alpine \
  tar xzf /backup/ngenorca-backup-YYYYMMDD.tar.gz -C /
```

---

## Authentication in Docker

### No Auth (Default)
Good for local-only access. The gateway accepts all requests.

### Token Auth
Set in your `.env`:
```env
NGENORCA_GATEWAY__AUTH_MODE=Token
NGENORCA_GATEWAY__AUTH_TOKENS=["my-secret-token"]
```

Then use:
```bash
curl -H "Authorization: Bearer my-secret-token" http://localhost:18789/api/v1/chat ...
```

### Behind a Reverse Proxy (Recommended for Remote Access)
See [docs/NAS_DEPLOYMENT.md](NAS_DEPLOYMENT.md) for the full nginx + Authelia setup.

---

## Resource Limits

The default compose file sets:
- **Memory:** 256MB max
- **CPU:** 2 cores max
- **Logs:** 10MB × 3 files (json rotation)

Adjust in `docker-compose.yml` under `deploy.resources.limits`.

---

## Troubleshooting

### Container won't start
```bash
docker logs ngenorca
```
Common causes:
- Port `18789` already in use → change `ports` in compose
- Invalid config → check `config/config.toml` syntax

### "connection refused" to Ollama
- Ensure Ollama is running: `curl http://localhost:11434/api/tags`
- On Docker Desktop: use `host.docker.internal` (already set)
- On Linux: use the host's IP or add `extra_hosts: ["host.docker.internal:host-gateway"]` to compose

### Health check failing
```bash
docker exec ngenorca wget -q --spider http://localhost:18789/health
```
If this works inside the container but not from the host, check port mapping.

### Rebuild from scratch
```bash
docker compose down -v          # Remove volumes too
docker compose build --no-cache
docker compose up -d
```

---

## Next Steps

- **NAS/Homelab deployment:** [docs/NAS_DEPLOYMENT.md](NAS_DEPLOYMENT.md) — nginx + Authelia + WireGuard
- **Full configuration reference:** [docs/CONFIGURATION_GUIDE.md](CONFIGURATION_GUIDE.md)
- **Channel setup (Telegram, Discord, etc.):** See the channels section in the config guide
