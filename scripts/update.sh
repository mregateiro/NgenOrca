#!/usr/bin/env bash
# If invoked via `sh` instead of `bash`, re-exec with bash to get full feature support.
if [ -z "${BASH_VERSION:-}" ]; then
  exec bash "$0" "$@"
fi
set -euo pipefail

MODE="${1:-docker}"
BRANCH="${2:-master}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

# ── Config migration helper ──────────────────────────────────────
# Patches known-bad defaults in existing config files so that users
# who simply run `update.sh` get the fixes without manual editing.
migrate_config() {
  local file="$1"
  [ -f "$file" ] || return 0

  local changed=false

  # v0.1.3: default bind address changed from 127.0.0.1 → 0.0.0.0
  # so the gateway is reachable from outside the host.
  if grep -qE '^bind\s*=\s*"127\.0\.0\.1"' "$file"; then
    sed -i.bak 's/^bind[[:space:]]*=[[:space:]]*"127\.0\.0\.1"/bind = "0.0.0.0"/' "$file"
    rm -f "${file}.bak"
    changed=true
  fi

  if [ "$changed" = true ]; then
    echo "    Updated: $file"
  fi
}

echo "==> Updating repository ($BRANCH)"
git fetch origin --prune
git checkout "$BRANCH"
git pull --ff-only origin "$BRANCH"

# ── Migrate existing config files ────────────────────────────────
echo "==> Checking config files for outdated defaults"
migrate_config "${HOME}/.ngenorca/config.toml"
migrate_config "/etc/ngenorca/config.toml"
migrate_config "/var/lib/ngenorca/config/config.toml"

case "$MODE" in
  docker)
    echo "==> Redeploying Docker stack"
    docker compose up -d --build
    ;;
  nas)
    echo "==> Redeploying NAS Docker stack"
    docker compose -f docker-compose.nas.yml up -d --build
    ;;
  cargo)
    echo "==> Reinstalling native CLI from current source"
    cargo install --path crates/ngenorca-cli --locked --force
    ;;
  *)
    echo "Unknown mode: $MODE"
    echo "Usage: ./scripts/update.sh [docker|nas|cargo] [branch]"
    exit 1
    ;;
esac

echo "==> Done"
