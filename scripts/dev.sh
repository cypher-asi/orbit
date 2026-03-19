#!/usr/bin/env bash
# Dev script: start Postgres via Docker Compose, then run the app.
# For WSL, Git Bash, macOS, Linux. Requires Docker.

set -e
LOCAL_DB_URL="postgres://postgres:postgres@localhost:5432/orbit"

# Check Docker is available
if ! command -v docker &>/dev/null; then
    echo "Docker is not installed or not in PATH. Install Docker and try again." >&2
    exit 1
fi
if ! docker info &>/dev/null; then
    echo "Docker daemon is not running. Start Docker and try again." >&2
    exit 1
fi

# Ensure we're in repo root (script lives in scripts/)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

echo "Starting Postgres via Docker Compose..."
docker compose up -d

# Wait for Postgres to be ready (timeout 30s)
timeout=30
elapsed=0
interval=2
while [ $elapsed -lt $timeout ]; do
    if docker compose exec -T db pg_isready -U postgres &>/dev/null; then
        break
    fi
    sleep $interval
    elapsed=$((elapsed + interval))
done
if [ $elapsed -ge $timeout ]; then
    echo "Postgres did not become ready within ${timeout}s." >&2
    exit 1
fi
echo "Postgres is ready."

# Load .env if present so other vars apply
if [ -f .env ]; then
    set -a
    # shellcheck source=/dev/null
    source .env
    set +a
fi

# Override DATABASE_URL for this run
export DATABASE_URL="$LOCAL_DB_URL"

echo "Running: cargo run"
exec cargo run
