<h1 align="center">ORBIT</h1>

<p align="center">
  <b>GitHub-lite Git Service</b><br>
  A lightweight super fast git server with a REST API for repos, branches, commits, pull requests, and merge, plus Git Smart HTTP for clone, fetch, and push.
</p>

<p align="center">
  <a href="#overview">Overview</a> · <a href="#quick-start">Quick Start</a> · <a href="#architecture">Architecture</a> · <a href="#principles">Principles</a> · <a href="#project-structure">Project Structure</a> · <a href="#api-endpoints">API Endpoints</a>
</p>

## Overview

Orbit is a self-hosted git server that exposes a REST API and Git Smart HTTP transport. It stores repository metadata in PostgreSQL and bare git repos on disk. Authentication is Bearer-only (login token or personal access token); read access is optional for public repos. The API is available at the root and under `/v1` for a stable versioned prefix. Optional Redis backs rate limiting across multiple instances.

---

## Core Concepts

1. **Repositories:** Top-level container owned by a user. Each repo has a slug, visibility (public or private), default branch, and optional description. Identified as `{owner}/{repo}` in paths; Git URLs use `{repo}.git` (e.g. `my-project.git`).

2. **Branches, commits, and tree:** Branches point at commits. The API lists commits (with optional ref), fetches a single commit and its diff, and browses the tree or fetches blob content by ref and path.

3. **Pull requests:** Create PRs from a source branch to a target branch, update title/description, close or reopen, fetch diff and mergeability and conflicts, and merge (merge or squash). PRs are identified by UUID in the path; the `number` field remains for display (e.g. "PR #5").

4. **Git HTTP transport:** Standard Git clients use `GET /{owner}/{repo}/info/refs` and `POST /{owner}/{repo}/git-upload-pack` (fetch/clone) and `git-receive-pack` (push). Paths include the `.git` suffix. Public repos allow anonymous read; private repos and push require Bearer auth.

5. **Auth:** Login (`POST /auth/login`) or register (`POST /auth/register`) yields a token; alternatively create personal access tokens (`POST /auth/tokens`). Use `Authorization: Bearer <token>` for API and Git HTTP. Optional on read endpoints; public repos allow anonymous read.

---

## Quick Start

### Prerequisites

- Rust toolchain
- PostgreSQL (connection URL for `DATABASE_URL`)
- Optional: Redis (for distributed rate limiting across instances)

### Configuration

Copy `.env.example` to `.env` and set at least `DATABASE_URL` and `GIT_STORAGE_ROOT`. For production, consider `CORS_ALLOWED_ORIGINS`, `PUBLIC_BASE_URL`, and `REDIS_URL`. See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for server and client (e.g. aura-app) setup.

### Run

```
cargo run
```

The server binds to `SERVER_HOST:SERVER_PORT` (default `0.0.0.0:3000`). Database migrations run on startup. Liveness: **GET /health**. Discovery (metadata for clients): **GET /** or **GET /api**.

---

## Principles

1. **Self-hosted:** Your data stays in your PostgreSQL and git storage. No cloud dependency; run Orbit on your own infrastructure.
2. **REST + Git HTTP:** Standard Git clients work via Smart HTTP. REST clients (e.g. aura-app) integrate using the discovery endpoint for base URL and clone URL prefix.
3. **Bearer-only auth:** Login or PAT; same token for REST and Git HTTP. Rate limiting (auth 10/min, token create 20/min, repo create / repo write / admin mutations / git push 30/min per IP) with optional Redis for multi-instance consistency.

---

## Architecture

Single Rust binary (Axum). Modules and responsibilities:

| Module | Description |
| --- | --- |
| **api** | Router composition, health check, discovery, rate-limit layers |
| **auth** | Login, PAT CRUD, password hashing, Bearer extraction, admin extractor |
| **users** | Registration, profile (GET/PATCH /users/me) |
| **repos** | Repository CRUD, list by user |
| **branches** | Branch list, create, get, delete |
| **commits** | Commit list, get, diff; tree and blob browsing |
| **tags** | Tag list (name + target SHA) |
| **pull_requests** | PR CRUD, close, reopen, diff, mergeability, conflicts |
| **merge_engine** | Merge PR (merge/squash strategies) |
| **permissions** | Collaborators list, add/update role, remove |
| **events** | Repo-scoped and admin audit events, logging init |
| **admin** | Admin users, repos, jobs, events (read and mutation routes) |
| **git_http** | Git Smart HTTP (info/refs, upload-pack, receive-pack) |
| **jobs** | Background job definitions and worker |
| **storage** | Git storage path and pack operations |
| **db** | Connection pool and migrations |
| **config** | Env-based configuration |
| **errors** | Shared error types and responses |

---

## Project Structure

```
orbit/
  Cargo.toml                 # Rust package
  .env.example               # Env template (DATABASE_URL, GIT_STORAGE_ROOT, etc.)
  src/
    main.rs                  # Entrypoint, migrations, server, job worker
    app_state.rs             # Shared Axum state
    config.rs                # Config load
    db.rs                    # Pool and migrations
    errors.rs                # Error handling
    api/                     # Router, health, discovery, rate_limit, pagination, response
    auth/                    # Login, tokens, middleware, extractors
    users/                   # Register, profile routes
    repos/                   # Repo CRUD routes and service
    branches/                # Branch routes and service
    commits/                 # Commits, tree, blob routes and service
    tags/                    # Tags routes and service
    pull_requests/           # PR routes and service
    merge_engine/            # Merge handler and strategies
    permissions/             # Collaborator routes and service
    events/                  # Event routes, service, logging
    admin/                   # Admin routes
    git_http/                # Git HTTP routes and service
    jobs/                    # Job models, service, worker
    storage/                 # Git storage service
  docs/
    API.md                   # Full API reference (bodies, errors, rate limiting)
    CONFIGURATION.md         # Server and client configuration
```

---

## API Endpoints

All REST routes are also mounted under **/v1** (e.g. **GET /v1/repos**). Git HTTP and **GET /health** stay at the root. Auth: `Authorization: Bearer <token>`; optional on read for public repos. Resource IDs are UUIDs.

**Full reference:** [docs/API.md](docs/API.md) — request/response bodies, errors, rate limiting.

### Discovery

| Method | Path | Description |
| --- | --- | --- |
| GET | / or /api | Server metadata (api_version, base_url, git_url_prefix, auth). No auth. |

### Health

| Method | Path | Description |
| --- | --- | --- |
| GET | /health | Liveness/readiness. No auth. |

### Authentication

| Method | Path | Description |
| --- | --- | --- |
| POST | /auth/register | Register user (username, email, password). |
| POST | /auth/login | Login (login + password). Returns token and user. |
| POST | /auth/tokens | Create PAT (name, optional expires_in_days). Rate-limited. |
| GET | /auth/tokens | List PATs (no raw token). |
| DELETE | /auth/tokens/{id} | Revoke PAT. |

### Users

| Method | Path | Description |
| --- | --- | --- |
| GET | /users/me | Current user (auth required). |
| PATCH | /users/me | Update profile (auth required). |
| GET | /users/{username}/repos | List repos for user. Optional auth; public only for others. |

### Repositories

| Method | Path | Description |
| --- | --- | --- |
| GET | /repos | List repos accessible to current user (auth). |
| POST | /repos | Create repo (auth). Rate-limited. |
| GET | /repos/{owner}/{repo} | Get repo metadata. Optional auth. |
| PATCH | /repos/{owner}/{repo} | Update repo (admin/owner). |
| DELETE | /repos/{owner}/{repo} | Soft-delete repo (admin/owner). |
| POST | /repos/{owner}/{repo}/archive | Archive repo (admin/owner). |

### Branches

| Method | Path | Description |
| --- | --- | --- |
| GET | /repos/{owner}/{repo}/branches | List branches. Optional auth. |
| POST | /repos/{owner}/{repo}/branches | Create branch (write). |
| GET | /repos/{owner}/{repo}/branches/{*branch} | Get branch. Optional auth. |
| DELETE | /repos/{owner}/{repo}/branches/{*branch} | Delete branch (write). |

### Commits and tree

| Method | Path | Description |
| --- | --- | --- |
| GET | /repos/{owner}/{repo}/commits | List commits (ref, limit, offset). |
| GET | /repos/{owner}/{repo}/commits/{sha} | Get commit. |
| GET | /repos/{owner}/{repo}/commits/{sha}/diff | Get commit diff. |
| GET | /repos/{owner}/{repo}/tree/{ref}/{*path} | Browse tree. Empty path = root. |
| GET | /repos/{owner}/{repo}/blob/{ref}/{*path} | Get file content. |

### Tags

| Method | Path | Description |
| --- | --- | --- |
| GET | /repos/{owner}/{repo}/tags | List tags (name + target SHA). Optional auth. |

### Collaborators

| Method | Path | Description |
| --- | --- | --- |
| GET | /repos/{owner}/{repo}/collaborators | List collaborators (admin/owner). |
| PUT | /repos/{owner}/{repo}/collaborators/{username} | Add or update role (reader/writer/owner). |
| DELETE | /repos/{owner}/{repo}/collaborators/{username} | Remove collaborator. |

### Pull requests

| Method | Path | Description |
| --- | --- | --- |
| GET | /repos/{owner}/{repo}/pulls | List PRs (status, author_id, limit, offset). |
| POST | /repos/{owner}/{repo}/pulls | Create PR (write). Rate-limited. |
| GET | /repos/{owner}/{repo}/pulls/{id} | Get PR (id = UUID). |
| PATCH | /repos/{owner}/{repo}/pulls/{id} | Update title/description (author or write). |
| POST | /repos/{owner}/{repo}/pulls/{id}/close | Close PR. |
| POST | /repos/{owner}/{repo}/pulls/{id}/reopen | Reopen PR. |
| GET | /repos/{owner}/{repo}/pulls/{id}/diff | Get PR diff (text). |
| GET | /repos/{owner}/{repo}/pulls/{id}/mergeability | Mergeability state. |
| GET | /repos/{owner}/{repo}/pulls/{id}/conflicts | Conflict check (files). |
| POST | /repos/{owner}/{repo}/pulls/{id}/merge | Merge PR (write). Rate-limited. |

### Repo events

| Method | Path | Description |
| --- | --- | --- |
| GET | /repos/{owner}/{repo}/events | Audit events for repo (event_type, since, until, limit, offset). Read auth. |

### Admin

All require admin authentication.

| Method | Path | Description |
| --- | --- | --- |
| GET | /admin/users | List users (limit, offset, username prefix). |
| GET | /admin/users/{id} | Get user. |
| POST | /admin/users/{id}/disable | Disable user. Rate-limited. |
| POST | /admin/users/{id}/enable | Enable user. Rate-limited. |
| GET | /admin/repos | List repos (limit, offset, search). |
| GET | /admin/repos/{id} | Get repo + storage_exists. |
| POST | /admin/repos/{id}/archive | Archive repo. Rate-limited. |
| GET | /admin/jobs | List jobs (limit, offset, status). |
| GET | /admin/jobs/failed | List failed jobs. |
| POST | /admin/jobs/{id}/retry | Retry job. Rate-limited. |
| GET | /admin/events | Audit events (actor_id, repo_id, event_type, since, until, limit, offset). |

### Git HTTP transport

Paths use `{repo}` **including** `.git` (e.g. `my-repo.git`). Clone URL: `https://<host>/<owner>/<repo>.git`.

| Method | Path | Description |
| --- | --- | --- |
| GET | /{owner}/{repo}/info/refs?service=git-upload-pack | Ref advertisement for fetch/clone. |
| GET | /{owner}/{repo}/info/refs?service=git-receive-pack | Ref advertisement for push. Auth + write. |
| POST | /{owner}/{repo}/git-upload-pack | Pack negotiation (fetch/clone). |
| POST | /{owner}/{repo}/git-receive-pack | Push. Auth + write; rejects archived. Rate-limited. |

---

## License

MIT
