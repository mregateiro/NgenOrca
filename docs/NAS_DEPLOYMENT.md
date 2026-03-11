# NAS / Homelab Deployment Guide

Deploy NgenOrca on your NAS behind WireGuard + nginx + Authelia for a fully private, authenticated AI assistant.

This guide assumes your NAS is **behind NAT** and **not exposed directly to the public internet**.
Clients reach it over **WireGuard first**, then nginx forwards requests internally to NgenOrca.

Important consequences of this setup:
- **Do not port-forward** NgenOrca's port `18789` to the internet.
- In the NAS compose file, NgenOrca uses `expose`, not `ports`, so it is reachable only from containers on the same Docker network.
- **Telegram should stay in polling mode** for this setup. Do not configure a Telegram webhook unless you deliberately expose a public HTTPS endpoint.
- Public-webhook channels such as WhatsApp or Teams need extra public ingress if you want inbound provider callbacks.

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

If the `proxy` Docker network does not already exist, create it once:

```bash
docker network create proxy
```

If nginx or Authelia use a different external Docker network name, replace `proxy` everywhere in this guide and in [docker-compose.nas.yml](docker-compose.nas.yml).

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

Notes:
- Leave `OPENAI_API_KEY` blank unless you actually use OpenAI.
- Leave `TELEGRAM_ENABLED=false` if you do not want Telegram.
- For a NAT-only NAS setup, **do not add a Telegram webhook URL**. The NAS compose file already forces Telegram polling mode.

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

Also confirm these names match your Docker environment:
- `proxy_pass http://ngenorca:18789;` → your NgenOrca container/service name must be `ngenorca`
- `proxy_pass http://authelia:9091/...;` → your Authelia container/service name must be `authelia`
- both containers must be attached to the same external Docker network as nginx

The key parts are:
- **Authelia auth_request** — validates the user before proxying to NgenOrca
- **Remote-User/Remote-Email/Remote-Groups headers** — passes the authenticated identity
- **WebSocket support** — `Upgrade` + `Connection` headers for real-time chat
- **Health endpoint bypass** — `/health` skips Authelia for Docker healthchecks

Reload nginx:
```bash
docker exec nginx nginx -s reload
```

If your nginx container is not literally named `nginx`, use its real container name instead.

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
# From inside the NgenOrca container
docker exec ngenorca wget -qO- http://localhost:18789/health

# Or from another container on the same Docker network
docker run --rm --network proxy curlimages/curl:8.7.1 http://ngenorca:18789/health

# Or from your VPN-connected laptop through nginx
curl https://ngenorca.nas.local/health
```

Why three different checks?
- `localhost:18789` works **inside** the NgenOrca container.
- `http://ngenorca:18789` works only for containers on the same Docker network.
- `https://ngenorca.nas.local` is the check you use from your WireGuard-connected client devices.

## Step 5: Verify Authentication Flow

From your phone or laptop (connected via WireGuard):

1. Open `https://ngenorca.nas.local` in your browser
2. Authelia should redirect you to `https://auth.nas.local`
3. Log in with your credentials + 2FA
4. You should see the NgenOrca root response or web UI, depending on your client/path

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

The NAS compose file already sets `NGENORCA_CHANNELS__TELEGRAM__POLLING=true`.
NgenOrca connects outbound to Telegram's servers — no inbound webhook needed.

For this NAT/WireGuard setup:
- leave Telegram `webhook_url` unset
- do not port-forward a Telegram webhook endpoint
- do not expose `18789` publicly just for Telegram

If you later move to a public HTTPS deployment, you can switch Telegram to webhook mode explicitly.

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
- Upstream name mismatch: your nginx config uses `ngenorca` and `authelia` as upstream names; rename them in the config if your container names differ

### "401 Unauthorized" from NgenOrca
- Auth mode mismatch: NgenOrca expects `TrustedProxy` but nginx isn't sending headers
- Header name mismatch: check `proxy_user_header` matches what nginx sends

### Authelia doesn't redirect
- Domain not in Authelia's `access_control` rules
- Browser DNS doesn't resolve `ngenorca.nas.local` — add to WireGuard's DNS or `/etc/hosts`
- `auth.nas.local` must also resolve on the client device, not just `ngenorca.nas.local`

### Telegram bot not responding
- Check `TELEGRAM_ENABLED=true` and `TELEGRAM_BOT_TOKEN` in `.env`
- View logs: `docker logs ngenorca | grep -i telegram`
- Verify token: `curl https://api.telegram.org/bot<YOUR_TOKEN>/getMe`
- For NAS/NAT mode, make sure you did **not** try to configure a Telegram webhook; polling is the intended setup here
