#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-docker}"
BRANCH="${2:-master}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

echo "==> Updating repository ($BRANCH)"
git fetch origin --prune
git checkout "$BRANCH"
git pull --ff-only origin "$BRANCH"

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
