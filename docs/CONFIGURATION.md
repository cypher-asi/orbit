# Orbit configuration for clients (e.g. aura-app)

This document describes how to configure Orbit and how clients such as aura-app can integrate with it.

## Server configuration

### Required

- **DATABASE_URL** — PostgreSQL connection URL.
- **GIT_STORAGE_ROOT** — Directory for bare Git repos (default: `./data/repos`).

### Recommended for production

- **CORS_ALLOWED_ORIGINS** — Comma-separated list of allowed origins. Set to your frontend origin(s) so the browser can call the Orbit API. Example: `https://app.example.com,https://aura.example.com`. When empty, any origin is allowed (suitable only for development).
- **PUBLIC_BASE_URL** — The public URL of the Orbit server (e.g. `https://orbit.example.com`). Used by the discovery endpoint (`GET /api`, `GET /v1`) so clients can build API and Git clone URLs. When unset, Orbit uses `http://SERVER_HOST:SERVER_PORT`.
- **REDIS_URL** — Optional. When set, rate limiting uses Redis so limits are shared across multiple Orbit instances.

See `.env.example` for all variables.

## Client integration (aura-app)

### Discovery

Point the client at the Orbit server and call **GET /** or **GET /api** (no auth). The response includes:

- `api_version` — e.g. `"1"`.
- `base_url` — Base for REST (e.g. `https://orbit.example.com`). Use `{base_url}/v1/repos`, etc.
- `git_url_prefix` — Base for clone URLs. Clone URL = `{git_url_prefix}{owner}/{repo}.git`.
- `auth` — `"bearer"`. Use `Authorization: Bearer <token>` for API and Git HTTP.

### Authentication

- **Login**: `POST {base_url}/auth/login` with `{ "login": "username or email", "password": "..." }`. Response includes `token`; use it as the Bearer token.
- **PAT**: `POST {base_url}/auth/tokens` with `{ "name", "expires_in_days?" }` (auth required). Store the returned `token` securely; it is shown only once.

### Git remote

Use `https://{host}/{owner}/{repo}.git` as the remote URL. For private repos, send the Bearer token (e.g. via Git credential helper or `https://username:TOKEN@host/owner/repo.git` if Orbit supports it).

### REST API

Use the versioned base path **/v1** for a stable contract: e.g. `GET {base_url}/v1/repos`, `GET {base_url}/v1/repos/{owner}/{repo}/tags`. Full reference: [API.md](API.md).

### Summary for aura-app

1. Configure Orbit base URL (from env or discovery).
2. Call `GET /api` to get `base_url` and `git_url_prefix`.
3. Use `POST /auth/login` or PAT creation to obtain a Bearer token.
4. Call REST at `{base_url}/v1/...` with `Authorization: Bearer <token>`.
5. Set CORS on Orbit to the aura-app frontend origin(s).
