# Deploying NgenOrca with Portainer

This guide walks you through setting up NgenOrca using Portainer's web UI — no terminal required. Works with Portainer CE (free) or Business Edition.

---

## Prerequisites

- **Portainer** ≥ 2.19 running and accessible (see [portainer.io/install](https://www.portainer.io/install))
- A Docker host managed by Portainer (local or remote agent)
- An LLM provider API key **or** [Ollama](https://ollama.com) running on the host

---

## Method 1: Stack (Recommended)

Portainer Stacks are the UI equivalent of `docker compose`. This is the easiest way.

### Step 1 — Create the Stack

1. Open Portainer → select your **Environment** (e.g., "local").
2. Go to **Stacks** → **+ Add stack**.
3. Name it `ngenorca`.
4. Under **Build method**, choose **Web editor**.
5. Paste the following compose definition:

```yaml
services:
  core:
    image: ghcr.io/ngenorca/ngenorca:latest
    # To build from source instead, comment out 'image' and uncomment:
    # build:
    #   context: https://github.com/mregateiro/NgenOrca.git
    container_name: ngenorca
    restart: unless-stopped
    command: ["ngenorca", "gateway", "--bind", "0.0.0.0", "--port", "18789", "--config", "/var/lib/ngenorca/config/config.toml"]
    ports:
      - "18789:18789"
    volumes:
      - ngenorca-data:/var/lib/ngenorca
      - ngenorca-config:/var/lib/ngenorca/config
    environment:
      - NGENORCA_DATA_DIR=/var/lib/ngenorca/data
      - NGENORCA_AGENT__WORKSPACE=/var/lib/ngenorca/workspace
      # Optional bootstrap overrides. Leave these commented if you want the
      # /config web UI to be the main source of truth.
      # - NGENORCA_AGENT__MODEL=${NGENORCA_MODEL:-anthropic/claude-sonnet-4-20250514}
      # - NGENORCA_AGENT__PROVIDERS__ANTHROPIC__API_KEY=${ANTHROPIC_API_KEY:-}
      # - NGENORCA_AGENT__PROVIDERS__OPENAI__API_KEY=${OPENAI_API_KEY:-}
      # - NGENORCA_AGENT__PROVIDERS__OLLAMA__BASE_URL=${OLLAMA_URL:-http://host.docker.internal:11434}
    healthcheck:
      test: ["CMD", "wget", "-q", "--spider", "http://localhost:18789/health"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s
    deploy:
      resources:
        limits:
          memory: 256M
          cpus: "2.0"
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"

volumes:
  ngenorca-data:
    driver: local
  ngenorca-config:
    driver: local
```

### Step 2 — Add Environment Variables

Scroll down to the **Environment variables** section (below the editor). Click **+ Add an environment variable** for each secret:

| Name | Value | Required? |
|------|-------|-----------|
| `NGENORCA_MODEL` | `anthropic/claude-sonnet-4-20250514` | Yes |
| `ANTHROPIC_API_KEY` | `sk-ant-api03-...` | If using Anthropic |
| `OPENAI_API_KEY` | `sk-...` | If using OpenAI |
| `OLLAMA_URL` | `http://host.docker.internal:11434` | If using Ollama |
| `TELEGRAM_BOT_TOKEN` | `123456:ABC-...` | If using Telegram |
| `DISCORD_BOT_TOKEN` | `MTIz...` | If using Discord |

> **Tip:** If you want Portainer's `/config` page to be authoritative, leave the `NGENORCA_*` overrides commented and save your settings through the web UI after first boot.

### Step 3 — Deploy

Click **Deploy the stack**. Portainer will pull the image (or build from source) and start the container.

### Step 4 — Verify

1. Go to **Containers** → click **ngenorca**.
2. Check the **Status** is `running` and the health check shows `healthy`.
3. Click **Logs** to see:
   ```
   NgenOrca gateway listening on 0.0.0.0:18789
   ```
4. Open `http://<your-host>:18789/health` in a browser — you should see `{"status":"ok"}`.
5. Open `http://<your-host>:18789/config`, save your config, then restart the container.

---

## Method 2: Git Repository Stack

If you want Portainer to pull directly from the NgenOrca repo (and build from source):

1. **Stacks** → **+ Add stack**.
2. Under **Build method**, choose **Repository**.
3. Fill in:
  - **Repository URL**: `https://github.com/mregateiro/NgenOrca`
  - **Reference**: `master`
   - **Compose path**: `docker-compose.yml`
4. Add environment variables as in Step 2 above.
5. Click **Deploy the stack**.

> **Note:** Building from source takes 2–5 minutes on the first deploy. Subsequent redeploys use the Docker cache.

---

## Method 3: Single Container (No Compose)

If you prefer to manage a single container without a stack:

1. Go to **Containers** → **+ Add container**.
2. Fill in:
   - **Name**: `ngenorca`
   - **Image**: `ghcr.io/ngenorca/ngenorca:latest`
3. Under **Network ports configuration**, click **+ publish a new network port**:
   - Host: `18789` → Container: `18789`
4. Under **Volumes** → **+ map additional volume**:
   - Container: `/var/lib/ngenorca` → Volume: `ngenorca-data` (create new)
  - Container: `/var/lib/ngenorca/config` → Volume: `ngenorca-config` (create new)
5. Under **Env** → **+ add environment variable**:
  - `NGENORCA_DATA_DIR` = `/var/lib/ngenorca/data`
  - `NGENORCA_AGENT__WORKSPACE` = `/var/lib/ngenorca/workspace`
6. Under **Restart policy**, select **Unless stopped**.
7. Click **Deploy the container**.
8. Open `http://<your-host>:18789/config`, save your config, then restart the container.

---

## Using Ollama on the Same Host

If Ollama runs on the same machine as your Docker host:

### Docker Desktop (macOS / Windows)
The special hostname `host.docker.internal` already resolves to the host. Set:
```
OLLAMA_URL = http://host.docker.internal:11434
NGENORCA_MODEL = ollama/llama3.1
```

### Linux Docker Host
Add `host.docker.internal` resolution. In the stack YAML, add under the `core` service:

```yaml
    extra_hosts:
      - "host.docker.internal:host-gateway"
```

Or use the host's LAN IP directly (e.g., `http://192.168.1.50:11434`).

---

## Custom Configuration File

To use a TOML config file instead of (or alongside) environment variables:

1. Go to **Volumes** → click on `ngenorca-config`.
2. Click **Browse** to open the volume contents.
3. Upload your `config.toml` file (based on `config.example.toml` from the repository).

Alternatively, use Portainer's **Configs** feature (Business Edition) or bind-mount a host directory:

```yaml
    volumes:
      - ngenorca-data:/var/lib/ngenorca
      - /path/to/your/config:/var/lib/ngenorca/config
```

---

## Enabling Authentication

### Token Auth (Simple)

Add these environment variables to the stack:

```
NGENORCA_GATEWAY__AUTH_MODE = Token
NGENORCA_GATEWAY__AUTH_TOKENS = ["my-secret-token-here"]
```

Then include the header in all requests:
```
Authorization: Bearer my-secret-token-here
```

### Behind a Reverse Proxy (Authelia / Authentik)

If Portainer and NgenOrca sit behind a reverse proxy with SSO:

1. Set auth mode to `TrustedProxy`:
   ```
   NGENORCA_GATEWAY__AUTH_MODE = TrustedProxy
   ```
2. The proxy must forward `Remote-User`, `Remote-Name`, and `Remote-Groups` headers.
3. **Do not** expose port `18789` publicly — only allow traffic from the reverse proxy network.

See [NAS_DEPLOYMENT.md](NAS_DEPLOYMENT.md) for a full nginx + Authelia example.

---

## Updating NgenOrca

### Stack (image-based)
1. Go to **Stacks** → click `ngenorca`.
2. Click **Editor** tab → **Update the stack** → check **Re-pull image and redeploy**.
3. Click **Update**.

### Stack (git-based)
1. Go to **Stacks** → click `ngenorca`.
2. Click **Editor** tab → **Pull and redeploy**.

### Single Container
1. Go to **Images** → pull `ghcr.io/ngenorca/ngenorca:latest`.
2. Go to **Containers** → **ngenorca** → **Recreate** → check **Pull latest image**.

---

## Backing Up Data

### Using Portainer UI

1. Go to **Volumes** → click `ngenorca-data` → **Browse**.
2. Download the SQLite database files manually.

### Using a Backup Container

Add a one-shot backup service to your stack:

```yaml
  backup:
    image: alpine:3.21
    volumes:
      - ngenorca-data:/data:ro
      - /path/to/backups:/backup
    command: >
      sh -c "tar czf /backup/ngenorca-$(date +%Y%m%d-%H%M%S).tar.gz -C /data ."
    profiles:
      - backup
```

Then in Portainer, redeploy the stack with the `backup` profile enabled, or run:
```bash
docker compose --profile backup run --rm backup
```

---

## Troubleshooting

### Container won't start
- **Logs**: Containers → ngenorca → Logs. Look for config errors or missing API keys.
- **Port conflict**: Change the host port from `18789` to something else in the port mapping.

### Health check "unhealthy"
- Click the container → **Console** → connect as `/bin/sh`.
- Run: `wget -q --spider http://localhost:18789/health`
- If it works inside but not outside, the issue is port mapping or firewall.

### Can't reach Ollama
- Verify Ollama is running: visit `http://<host-ip>:11434/api/tags` in your browser.
- On Linux, ensure `extra_hosts` is set (see Ollama section above).
- If running Ollama in Docker too, put both containers on the same Docker network.

### Portainer on ARM (Raspberry Pi / Apple Silicon)
NgenOrca publishes multi-arch images (`linux/amd64` and `linux/arm64`). Portainer will pull the correct architecture automatically.

### Rebuild from scratch
1. Go to **Stacks** → delete `ngenorca`.
2. Go to **Volumes** → delete `ngenorca-data` (⚠️ this erases all memory and identity data).
3. Recreate the stack from Step 1.

---

## Next Steps

- **Docker CLI guide**: [docs/DOCKER.md](DOCKER.md)
- **NAS / homelab with nginx + Authelia**: [docs/NAS_DEPLOYMENT.md](NAS_DEPLOYMENT.md)
- **Full configuration reference**: [docs/CONFIGURATION_GUIDE.md](CONFIGURATION_GUIDE.md)
- **Channel adapters (Telegram, Discord, Slack, etc.)**: see `config.example.toml`
