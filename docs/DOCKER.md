# Docker Quick Start Guide

Run NgenOrca in Docker with a persistent external config file that survives image redeploys.

---

## How config persistence works

- The repo keeps only templates such as [config/config.example.toml](../config/config.example.toml).
- The running container stores its editable config at `/var/lib/ngenorca/config/config.toml`.
- [docker-compose.yml](../docker-compose.yml) mounts that path from the persistent `ngenorca-config` Docker volume.
- The built-in browser page at `http://localhost:18789/config` reads and writes that file.
- Redeploying a new image replaces the container, not the `ngenorca-config` volume.

If you also set `NGENORCA_*` environment variables in compose, they still override the file. Leave them commented if you want `/config` to be the main source of truth.

---

## Step 1: Clone the repository

```bash
git clone https://github.com/mregateiro/NgenOrca.git ngenorca
cd ngenorca
```

## Step 2: Start the stack

```bash
docker compose up -d
```

This creates two persistent Docker volumes:

- `ngenorca-data` → databases, memory, learned routes
- `ngenorca-config` → the editable `config.toml`

## Step 3: Open the config UI

Open:

```text
http://localhost:18789/config
```

On first run, if no config file exists yet, the page pre-fills the editor with the current runtime config so you can save it as your starting point.

## Step 4: Paste your config

### Cloud provider example

```toml
[agent]
model = "anthropic/claude-sonnet-4-20250514"

[agent.providers.anthropic]
api_key = "sk-ant-api03-your-key-here"

[channels.webchat]
enabled = true
```

### Fully local Ollama example

```toml
[agent]
model = "ollama/llama3.1"

[agent.providers.ollama]
base_url = "http://host.docker.internal:11434"

[channels.webchat]
enabled = true
```

On Linux, replace `host.docker.internal` with your host IP or add an `extra_hosts` entry.

## Step 5: Save and restart

After clicking **Save config** in the browser UI, restart NgenOrca so it reloads the file:

```bash
docker restart ngenorca
```

## Step 6: Verify it works

```bash
curl http://localhost:18789/health
curl http://localhost:18789/api/v1/status
curl http://localhost:18789/api/v1/providers
```

## Step 7: Send your first message

```bash
curl -s -X POST http://localhost:18789/api/v1/chat \
	-H "Content-Type: application/json" \
	-d '{"message": "Hello! What can you do?", "channel": "webchat"}'
```

---

## Data persistence

| Volume | Container path | Contents |
|--------|----------------|----------|
| `ngenorca-data` | `/var/lib/ngenorca` | SQLite databases, memory, learned routes |
| `ngenorca-config` | `/var/lib/ngenorca/config` | Persistent `config.toml` and automatic backups |

## Common operations

### View logs

```bash
docker logs -f ngenorca
```

### Update to a new version

```bash
git pull
docker compose up -d --build
```

### Rebuild from scratch

```bash
docker compose down -v
docker compose build --no-cache
docker compose up -d
```

---

## Troubleshooting

### Config changes do not apply

- Saving through `/config` updates the persistent file, not the already-running process.
- Restart after each save: `docker restart ngenorca`
- If you added `NGENORCA_*` variables to [docker-compose.yml](../docker-compose.yml), those override the file until you remove them.

### Config does not persist across redeploys

- Check that the `ngenorca-config` volume exists: `docker volume inspect ngenorca-config`
- Do not edit files inside the image filesystem; use `/config`
- Do not remove volumes unless you explicitly want a reset (`docker compose down -v`)

### Ollama connection refused

- Ensure Ollama is running on the host
- On Linux, use the host IP or add Docker host-gateway mapping
- Verify the value saved in `/config` under `[agent.providers.ollama]`

---

## Next steps

- [docs/NAS_DEPLOYMENT.md](NAS_DEPLOYMENT.md) for WireGuard + nginx + Authelia
- [docs/PORTAINER.md](PORTAINER.md) for Portainer-based deployment
- [config/config.example.toml](../config/config.example.toml) for the full config template
