# NAS / Homelab Deployment Guide

Deploy NgenOrca on your NAS behind WireGuard + nginx + Authelia for a fully private, authenticated AI assistant.

This guide assumes your NAS is **behind NAT** and **not exposed directly to the public internet**.
Clients reach it over **WireGuard first**, then nginx forwards requests internally to NgenOrca.

Important consequences of this setup:
- **Do not port-forward** NgenOrca's port `18789` to the internet.
- In the NAS compose file, NgenOrca uses `expose`, not `ports`, so it is reachable only from containers on the same Docker network.
- The editable runtime config lives in a persistent Docker volume, so redeploying the image does not replace your settings.
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

If nginx or Authelia use a different external Docker network name, replace `proxy` everywhere in this guide and in [docker-compose.nas.yml](../docker-compose.nas.yml).

## Workflow 1: New installation

### Step 1: Clone and configure

```bash
# On your NAS, via SSH or terminal
git clone https://github.com/mregateiro/NgenOrca.git ngenorca
cd ngenorca
```

For Docker/NAS deployments, the real editable config file is persisted at:

```text
/var/lib/ngenorca/config/config.toml
```

You will create it from the built-in `/config` page after the stack starts.

If you want to prepare the values you will paste there, start from something like this:

```toml
[gateway]
auth_mode = "TrustedProxy"

[agent]
model = "anthropic/claude-sonnet-4-20250514"

[agent.providers.anthropic]
api_key = "sk-ant-api03-..."

[channels.telegram]
enabled = true
bot_token = "7123456789:AAH..."
polling = true
```

Notes:
- For a NAT-only NAS setup, **do not add a Telegram webhook URL**. The NAS compose file already forces Telegram polling mode.
- The NAS compose file bootstraps `TrustedProxy` via environment variables so the `/config` page is still protected by Authelia before `config.toml` exists.

### Step 2: nginx configuration

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

### Step 3: Authelia access rule

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

### Step 4: Start NgenOrca

```bash
# Run this from the cloned repo root (the folder that contains docker-compose.nas.yml)
docker compose -f docker-compose.nas.yml up -d
```

Then open `https://ngenorca.nas.local/config`, paste your TOML config, click **Save config**, and restart NgenOrca:

```bash
docker compose -f docker-compose.nas.yml restart ngenorca
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

### Step 5: Verify authentication flow

From your phone or laptop (connected via WireGuard):

1. Open `https://ngenorca.nas.local` in your browser
2. Authelia should redirect you to `https://auth.nas.local`
3. Log in with your credentials + 2FA
4. You should see the NgenOrca response or the `/config` editor, depending on your path

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

```toml
[agent]
model = "ollama/llama3.1"

[agent.providers.ollama]
base_url = "http://host.docker.internal:11434"
```

Or if Ollama is also in Docker on the same network:
```toml
[agent]
model = "ollama/llama3.1"

[agent.providers.ollama]
base_url = "http://ollama:11434"
```

This gives you a **100% local, zero-cloud** setup. No API keys needed, no data leaves your network.

## Telegram via Polling (No Public URL)

Since your NAS is behind WireGuard with no public IP, keep Telegram in polling mode inside `config.toml`:

```toml
[channels.telegram]
enabled = true
bot_token = "7123456789:AAHk..."
polling = true
```

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
  ├── config/
  │   ├── config.toml           # Saved by the built-in /config editor
  │   └── config.backup-*.toml  # Automatic backups before overwrite
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
| Runtime config | Persistent Docker volume — redeploying the image does not replace `config.toml` |
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
./scripts/update.sh nas
```

PowerShell:

```powershell
.\scripts\update.ps1 nas
```

## Workflow 2: Update existing installation

From the existing `ngenorca` repo folder on the NAS:

```bash
./scripts/update.sh nas
```

PowerShell:

```powershell
.\scripts\update.ps1 nas
```

What this does:

1. fetches the latest changes from GitHub
2. updates your local `master` branch
3. rebuilds and redeploys the NAS stack

Use this workflow for normal upgrades. Re-cloning is only for the first deploy.

## Troubleshooting

### "502 Bad Gateway" from nginx
- NgenOrca container isn't running: `docker ps | grep ngenorca`
- Wrong network: `docker network inspect proxy` — is ngenorca there?
- Port mismatch: check `expose: ["18789"]` in compose and `proxy_pass http://ngenorca:18789` in nginx
- Upstream name mismatch: your nginx config uses `ngenorca` and `authelia` as upstream names; rename them in the config if your container names differ

### Config changes do not survive a redeploy
- Make sure the `ngenorca-config` volume still exists: `docker volume inspect ngenorca-config`
- Save changes through `/config`, not by editing files inside the running container image
- Restart after each save: `docker compose -f docker-compose.nas.yml restart ngenorca`
- If you later add `NGENORCA_*` variables to the compose file, those values override the persisted TOML until you remove them

### "401 Unauthorized" from NgenOrca
- Auth mode mismatch: NgenOrca expects `TrustedProxy` but nginx isn't sending headers
- Header name mismatch: check `proxy_user_header` matches what nginx sends

### Authelia doesn't redirect
- Domain not in Authelia's `access_control` rules
- Browser DNS doesn't resolve `ngenorca.nas.local` — add to WireGuard's DNS or `/etc/hosts`
- `auth.nas.local` must also resolve on the client device, not just `ngenorca.nas.local`

### Telegram bot not responding
- Check that `[channels.telegram]` is enabled in `/config`
- View logs: `docker logs ngenorca | grep -i telegram`
- Verify token: `curl https://api.telegram.org/bot<YOUR_TOKEN>/getMe`
- For NAS/NAT mode, make sure you did **not** try to configure a Telegram webhook; polling is the intended setup here
