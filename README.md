<h1 align="center">ORBIT</h1>

<p align="center">
  <b>A git-based repository system for machines.</b>
</p>

## Overview

Orbit is the git hosting layer for the AURA platform. It stores repository metadata in PostgreSQL and bare git repos on disk. All AURA clients (desktop, web, mobile) and aura-swarm connect to orbit for code storage, commits, branches, PRs, and merges.

Repos are linked to [aura-network](https://github.com/cypher-asi/aura-network) orgs and projects via cross-service UUIDs. Authentication uses the same zOS JWT tokens as all other AURA services.

GitHub mirror is supported as a secondary/backup — when an org has a GitHub integration configured in aura-network, orbit mirrors pushes to the configured GitHub repo.

---

## Quick Start

### Prerequisites

- Rust toolchain
- PostgreSQL
- `git` CLI (orbit shells out to git for bare repo operations)
- Optional: Redis (for distributed rate limiting)

### Setup

```
cp .env.example .env
# Edit .env with your database URL and auth config

cargo run
```

The server binds to `0.0.0.0:3000` by default. Migrations run on startup.

### Health Check

```
curl http://localhost:3000/health
```

### Environment Variables

| Variable | Required | Description |
|---|---|---|
| `DATABASE_URL` | Yes | PostgreSQL connection string |
| `GIT_STORAGE_ROOT` | No | Path for bare git repos (default: `./data/repos`) |
| `AUTH0_DOMAIN` | Yes | Auth0 domain for JWKS |
| `AUTH0_AUDIENCE` | Yes | Auth0 audience identifier |
| `AUTH_COOKIE_SECRET` | Yes | Shared secret for HS256 token validation (same as aura-network/storage) |
| `INTERNAL_SERVICE_TOKEN` | Yes | Token for service-to-service auth (X-Internal-Token) |
| `AURA_NETWORK_URL` | No | aura-network base URL for GitHub mirror integration lookups |
| `SERVER_HOST` | No | Bind address (default: `0.0.0.0`) |
| `SERVER_PORT` | No | Bind port (default: `3000`) |
| `CORS_ORIGINS` | No | Comma-separated allowed origins. Omit for permissive (dev mode) |
| `REDIS_URL` | No | Redis URL for distributed rate limiting |
| `PUBLIC_BASE_URL` | No | Public URL for discovery endpoint |

---

## Authentication

All API endpoints require a JWT in the `Authorization: Bearer <token>` header. Same zOS tokens as aura-network and aura-storage — both RS256 (Auth0 JWKS) and HS256 (shared secret) are accepted.

For Git HTTP (clone/push), the JWT is passed as the password in Basic auth. Username can be anything.

```
git clone https://x-token:JWT_HERE@orbit.example.com/{org_id}/{repo}.git
```

Internal (service-to-service) endpoints use `X-Internal-Token` header.

---

## API Reference

All REST routes are also available under `/v1` (e.g. `GET /v1/repos`). Resource IDs are UUIDs. Responses use **camelCase** JSON.

### Discovery

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/` or `/api` | Server metadata (apiVersion, baseUrl, gitUrlPrefix) | None |

### Health

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/health` | Liveness/readiness check | None |

### Repositories

Repos are scoped by org. Each repo has an `orgId`, `projectId`, and `ownerId` linking back to aura-network.

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/repos` | List repos accessible to current user | JWT |
| POST | `/repos` | Create repo. Body: `{"orgId": "...", "projectId": "...", "name": "...", "visibility": "public"}` | JWT |
| GET | `/repos/{org_id}/{repo}` | Get repo metadata | JWT (optional for public) |
| PATCH | `/repos/{org_id}/{repo}` | Update repo (owner) | JWT |
| DELETE | `/repos/{org_id}/{repo}` | Soft-delete repo (owner) | JWT |
| POST | `/repos/{org_id}/{repo}/archive` | Archive repo (owner) | JWT |

### Branches

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/repos/{org_id}/{repo}/branches` | List branches | JWT (optional for public) |
| POST | `/repos/{org_id}/{repo}/branches` | Create branch | JWT (write) |
| GET | `/repos/{org_id}/{repo}/branches/{*branch}` | Get branch | JWT (optional for public) |
| DELETE | `/repos/{org_id}/{repo}/branches/{*branch}` | Delete branch | JWT (write) |

### Commits & Tree

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/repos/{org_id}/{repo}/commits` | List commits. Params: `?ref=&limit=&offset=` | JWT (optional for public) |
| GET | `/repos/{org_id}/{repo}/commits/{sha}` | Get commit | JWT (optional for public) |
| GET | `/repos/{org_id}/{repo}/commits/{sha}/diff` | Get commit diff | JWT (optional for public) |
| GET | `/repos/{org_id}/{repo}/tree/{ref}/{*path}` | Browse tree | JWT (optional for public) |
| GET | `/repos/{org_id}/{repo}/blob/{ref}/{*path}` | Get file content | JWT (optional for public) |

### Tags

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/repos/{org_id}/{repo}/tags` | List tags. Params: `?limit=&offset=` | JWT (optional for public) |

### Pull Requests

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/repos/{org_id}/{repo}/pulls` | List PRs. Params: `?status=&limit=&offset=` | JWT (optional for public) |
| POST | `/repos/{org_id}/{repo}/pulls` | Create PR. Body: `{"sourceBranch": "...", "targetBranch": "...", "title": "..."}` | JWT (write) |
| GET | `/repos/{org_id}/{repo}/pulls/{id}` | Get PR (id = UUID) | JWT (optional for public) |
| PATCH | `/repos/{org_id}/{repo}/pulls/{id}` | Update title/description | JWT (author or write) |
| POST | `/repos/{org_id}/{repo}/pulls/{id}/close` | Close PR | JWT (author or write) |
| POST | `/repos/{org_id}/{repo}/pulls/{id}/reopen` | Reopen PR | JWT (author or write) |
| GET | `/repos/{org_id}/{repo}/pulls/{id}/diff` | Get PR diff | JWT (optional for public) |
| GET | `/repos/{org_id}/{repo}/pulls/{id}/mergeability` | Mergeability check | JWT (optional for public) |
| GET | `/repos/{org_id}/{repo}/pulls/{id}/conflicts` | Conflict check | JWT (optional for public) |
| POST | `/repos/{org_id}/{repo}/pulls/{id}/merge` | Merge PR. Body: `{"strategy": "merge"}` | JWT (write) |

Merge strategies: `merge` (merge commit), `squash` (squash and merge).

### Collaborators

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/repos/{org_id}/{repo}/collaborators` | List collaborators | JWT (owner) |
| PUT | `/repos/{org_id}/{repo}/collaborators/{user_id}` | Add/update collaborator. Body: `{"role": "writer"}` | JWT (owner) |
| DELETE | `/repos/{org_id}/{repo}/collaborators/{user_id}` | Remove collaborator | JWT (owner) |

Roles: `owner`, `writer`, `reader`.

### Repo Events

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/repos/{org_id}/{repo}/events` | Audit events. Params: `?event_type=&since=&until=&limit=&offset=` | JWT (owner) |

### Admin

Authenticated via `X-Internal-Token` header.

| Method | Path | Description |
|---|---|---|
| GET | `/admin/repos` | List all repos |
| GET | `/admin/repos/{id}` | Get repo + storage status |
| POST | `/admin/repos/{id}/archive` | Archive repo |
| GET | `/admin/jobs` | List jobs |
| GET | `/admin/jobs/failed` | List failed jobs |
| POST | `/admin/jobs/{id}/retry` | Retry job |
| GET | `/admin/events` | Audit events (global) |

### Internal

Authenticated via `X-Internal-Token` header. Called by aura-network for auto-repo creation.

| Method | Path | Description |
|---|---|---|
| POST | `/internal/repos` | Auto-create repo. Body: `{"orgId": "...", "projectId": "...", "ownerId": "...", "name": "...", "visibility": "public"}` |

### Git HTTP Transport

Git clone/push uses JWT as password in Basic auth. Paths use `{org_id}` and `{repo}` includes `.git` suffix.

Clone URL: `https://x-token:JWT@host/{org_id}/{repo}.git`

| Method | Path | Description | Auth |
|---|---|---|---|
| GET | `/{org_id}/{repo}/info/refs?service=git-upload-pack` | Ref advertisement (clone/fetch) | Optional for public |
| GET | `/{org_id}/{repo}/info/refs?service=git-receive-pack` | Ref advertisement (push) | JWT (write) |
| POST | `/{org_id}/{repo}/git-upload-pack` | Clone/fetch | Optional for public |
| POST | `/{org_id}/{repo}/git-receive-pack` | Push | JWT (write) |

On successful push, orbit automatically:
1. Creates a **push post** in the aura-network feed (`POST /internal/activity`) with commit SHAs, push ID, and project/org context
2. Mirrors to GitHub if a GitHub integration is configured for the org

To track which agent performed the push, pass the `X-Agent-Id` header with the agent's UUID. Both `agentId` and `userId` (from JWT) are recorded on the feed post as a pair.

---

## Request/Response Format

All responses use JSON with **camelCase** field names.

**Successful responses:** 200 with JSON body, or 204 No Content for DELETE operations.

**Error responses:**
```json
{
  "error": {
    "code": "NOT_FOUND",
    "message": "repository not found",
    "details": null
  }
}
```

Error codes: `VALIDATION_ERROR` (400), `UNAUTHORIZED` (401), `FORBIDDEN` (403), `NOT_FOUND` (404), `CONFLICT` (409), `INTERNAL_ERROR` (500).

---

## GitHub Mirror

When an org has a GitHub integration configured in aura-network, orbit automatically mirrors pushes to the configured GitHub repo. This provides a secondary/backup for code storage.

Setup:
1. Configure a GitHub integration in aura-network: `POST /api/orgs/:id/integrations` with `{"integrationType": "github", "config": {"owner": "...", "repo": "...", "token": "ghp_..."}}`
2. Set `AURA_NETWORK_URL` in orbit's environment
3. Pushes to orbit will automatically mirror to GitHub

---

## Cross-Service References

Repos store UUIDs that reference entities in aura-network. These are **not** foreign key constrained (different databases).

| Field | References |
|---|---|
| `org_id` | Organization in aura-network |
| `project_id` | Project in aura-network |
| `owner_id` | User (zero user UUID) |

---

## License

MIT
