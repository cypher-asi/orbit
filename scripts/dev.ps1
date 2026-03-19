# Dev script: start Postgres via Docker Compose, then run the app.
# Requires Docker (Docker Desktop on Windows) to be installed and running.

$ErrorActionPreference = "Stop"
$LocalDbUrl = "postgres://postgres:postgres@localhost:5432/orbit"

# Check Docker is available
try {
    docker info 2>$null | Out-Null
} catch {
    Write-Error "Docker is not running or not installed. Start Docker Desktop and try again."
    exit 1
}

# Ensure we're in repo root (script lives in scripts/)
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Split-Path -Parent $ScriptDir
Set-Location $RepoRoot

# Start Postgres
Write-Host "Starting Postgres via Docker Compose..."
docker compose up -d
if ($LASTEXITCODE -ne 0) {
    Write-Error "docker compose up -d failed."
    exit 1
}

# Wait for Postgres to be ready (timeout 30s)
$timeout = 30
$elapsed = 0
$interval = 2
while ($elapsed -lt $timeout) {
    $result = docker compose exec -T db pg_isready -U postgres 2>$null
    if ($LASTEXITCODE -eq 0) {
        break
    }
    Start-Sleep -Seconds $interval
    $elapsed += $interval
}
if ($elapsed -ge $timeout) {
    Write-Error "Postgres did not become ready within ${timeout}s."
    exit 1
}
Write-Host "Postgres is ready."

# Load .env if present so other vars (GIT_STORAGE_ROOT, LOG_LEVEL, etc.) apply
if (Test-Path ".env") {
    Get-Content ".env" | ForEach-Object {
        if ($_ -match '^\s*([^#][^=]+)=(.*)$') {
            $name = $matches[1].Trim()
            $value = $matches[2].Trim()
            Set-Item -Path "Env:$name" -Value $value -Force
        }
    }
}

# Override DATABASE_URL for this run
$env:DATABASE_URL = $LocalDbUrl

Write-Host "Running: cargo run"
cargo run
