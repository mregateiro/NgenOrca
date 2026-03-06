# NAS / Homelab Deployment Guide

Deploy NgenOrca on your NAS behind WireGuard + nginx + Authelia for a fully private, authenticated AI assistant.

---

## Architecture

```
  Phone / Laptop (via WireGuard VPN)
         │
         │  encrypted tunnel
         ▼
┌───────────────────────────── NAS ──────────────────────────────┐
│                                                                │
│  nginx (:443)                                                  │
│    ├── auth.nas.local        → Authelia (:9091)                │
│    ├── ngenorca.nas.local    → Authelia → NgenOrca (:18789)    │
│    └── (your other services...)                                │
│                                                                │
│  NgenOrca container                                            │
│    ├── API keys              (env vars, never on disk)         │
│    ├── SQLite databases      (events, identity, memory)        │
│    ├── Sandbox               (Container mode)                  │
│    └── LLM calls ──────────► Anthropic / OpenAI (internet)     │
│              or ──────────► Ollama (local, on same NAS)        │
│                                                                │
│  Telegram / Discord          (polling, no public URL needed)   │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

## Prerequisites

- A NAS or Linux server with Docker
- WireGuard VPN configured (you already have this)
- nginx reverse proxy running in Docker
- Authelia running in Docker (you already have this)
- All three on the same Docker network (typically called `proxy`)

## Step 1: Clone and Configure

```bash
# On your NAS, via SSH or terminal
git clone https://github.com/ngenorca/ngenorca.git
cd ngenorca

# Create your secrets file
cp .env.example .env
nano .env
```

Fill in your `.env`:

```env
# Your Anthropic key (or OpenAI, or leave blank for Ollama-only)
ANTHROPIC_API_KEY=sk-ant-api03-...

# Model to use
NGENORCA_MODEL=anthropic/claude-sonnet-4-20250514

# Telegram bot (optional)
TELEGRAM_ENABLED=true
TELEGRAM_BOT_TOKEN=7123456789:AAH...

# Discord bot (optional)
DISCORD_ENABLED=false
DISCORD_BOT_TOKEN=
```

## Step 2: nginx Configuration

Copy the provided nginx config:

```bash
cp deploy/nginx/ngenorca.conf /path/to/your/nginx/conf.d/ngenorca.conf
```

Edit it to match your setup:

```nginx
server_name ngenorca.nas.local;        # ← Your internal domain
ssl_certificate     /path/to/cert.pem; # ← Your cert
ssl_certificate_key /path/to/key.pem;  # ← Your key
```

The key parts are:
- **Authelia auth_request** — validates the user before proxying to NgenOrca
- **Remote-User/Remote-Email/Remote-Groups headers** — passes the authenticated identity
- **WebSocket support** — `Upgrade` + `Connection` headers for real-time chat
- **Health endpoint bypass** — `/health` skips Authelia for Docker healthchecks

Reload nginx:
```bash
docker exec nginx nginx -s reload
```

## Step 3: Authelia Access Rule

Add NgenOrca to your Authelia `configuration.yml`:

```yaml
access_control:
  default_policy: deny
  rules:
    # ... your existing rules ...

    - domain: ngenorca.nas.local
      policy: two_factor             # Require 2FA
      subject:
        - "group:admins"             # Only your admin group
```

Restart Authelia:
```bash
docker restart authelia
```

## Step 4: Start NgenOrca

```bash
docker compose -f docker-compose.nas.yml up -d
```

Verify it's running:
```bash
# Health check (no auth needed)
curl http://ngenorca:18789/health

# Or from your VPN-connected laptop
curl https://ngenorca.nas.local/health
```

## Step 5: Verify Authentication Flow

From your phone or laptop (connected via WireGuard):

1. Open `https://ngenorca.nas.local` in your browser
2. Authelia should redirect you to `https://auth.nas.local`
3. Log in with your credentials + 2FA
4. You should see the NgenOrca API response

Check the `/api/v1/whoami` endpoint to verify identity passthrough:

```bash
# After logging into Authelia, your browser has the session cookie.
# Or test directly by simulating the proxy headers:
curl -H "Remote-User: miguel" \
     -H "Remote-Email: miguel@nas.local" \
     -H "Remote-Groups: admins" \
     http://ngenorca:18789/api/v1/whoami
```

Expected response:
```json
{
  "username": "miguel",
  "email": "miguel@nas.local",
  "groups": ["admins"],
  "auth_method": "TrustedProxy"
}
```

## Using with Ollama on the Same NAS

If you run Ollama on the NAS too:

```env
# .env
NGENORCA_MODEL=ollama/llama3.1
OLLAMA_URL=http://host.docker.internal:11434
```

Or if Ollama is also in Docker on the same network:
```env
OLLAMA_URL=http://ollama:11434
```

This gives you a **100% local, zero-cloud** setup. No API keys needed, no data leaves your network.

## Telegram via Polling (No Public URL)

Since your NAS is behind WireGuard with no public IP, use polling mode:

```env
TELEGRAM_ENABLED=true
TELEGRAM_BOT_TOKEN=7123456789:AAHk...
```

The config already sets `polling = true`. NgenOrca connects outbound to Telegram's servers — no inbound webhook needed.

## File Structure on NAS

```
/path/to/ngenorca/
├── docker-compose.nas.yml      # NAS-specific compose
├── .env                        # Your secrets (git-ignored)
├── config/
│   └── config.example.toml     # Reference — env vars override everything
├── deploy/
│   └── nginx/
│       └── ngenorca.conf       # nginx site config with Authelia
└── (source code...)
```

Docker volumes:
```
ngenorca-data → /var/lib/ngenorca/
  ├── events.db                 # Event bus (durable log)
  ├── identity.db               # User identities & device bindings
  └── memory/
      ├── episodic.db           # Conversation history
      └── semantic.db           # Distilled facts & knowledge
```

## Security Summary

| Layer | What Protects It |
|-------|-----------------|
| Network access | WireGuard VPN — only your devices can reach the NAS |
| Authentication | Authelia — 2FA before any request reaches NgenOrca |
| Identity | Remote-User header — NgenOrca knows who's talking |
| API keys | Environment variables — never written to disk, never in config files |
| Data at rest | SQLite on NAS filesystem — encrypted if your NAS has disk encryption |
| LLM traffic | HTTPS to provider APIs (Anthropic/OpenAI) |
| Tool execution | Container sandbox — Docker's own cgroups/namespaces |

## Useful Commands

```bash
# View logs
docker logs -f ngenorca

# Check status
curl https://ngenorca.nas.local/api/v1/status

# See configured providers
curl https://ngenorca.nas.local/api/v1/providers

# See configured channels
curl https://ngenorca.nas.local/api/v1/channels

# Restart
docker compose -f docker-compose.nas.yml restart

# Update
git pull
docker compose -f docker-compose.nas.yml up -d --build
```

## Troubleshooting

### "502 Bad Gateway" from nginx
- NgenOrca container isn't running: `docker ps | grep ngenorca`
- Wrong network: `docker network inspect proxy` — is ngenorca there?
- Port mismatch: check `expose: ["18789"]` in compose and `proxy_pass http://ngenorca:18789` in nginx

### "401 Unauthorized" from NgenOrca
- Auth mode mismatch: NgenOrca expects `TrustedProxy` but nginx isn't sending headers
- Header name mismatch: check `proxy_user_header` matches what nginx sends

### Authelia doesn't redirect
- Domain not in Authelia's `access_control` rules
- Browser DNS doesn't resolve `ngenorca.nas.local` — add to WireGuard's DNS or `/etc/hosts`

### Telegram bot not responding
- Check `TELEGRAM_ENABLED=true` and `TELEGRAM_BOT_TOKEN` in `.env`
- View logs: `docker logs ngenorca | grep -i telegram`
- Verify token: `curl https://api.telegram.org/bot<YOUR_TOKEN>/getMe`
