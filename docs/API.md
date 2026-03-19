# Orbit API Reference

Orbit is a GitHub-lite git service: REST API for repos, branches, commits, pull requests, and Git Smart HTTP for clone/fetch/push.

- **Base URL**: No version prefix at root; versioned API under `/v1` (see [Discovery](#discovery)).
- **Auth**: `Authorization: Bearer <token>` (PAT or login token). Optional on read endpoints; public repos allow anonymous read.
- **Identifiers**: `owner` = username, `repo` = repository slug (e.g. `my-project`). Git URLs use `{repo}.git` (e.g. `my-project.git`). Resource IDs (repos, users, pull requests, tokens, jobs, events, etc.) are **UUIDs** throughout the API.

---

## Discovery

**GET /** or **GET /api**

Returns server metadata for client configuration (e.g. aura-app). No auth required.

**Response** (JSON):

| Field | Type | Description |
|-------|------|-------------|
| `api_version` | string | e.g. `"1"` |
| `base_url` | string | Base URL for REST (e.g. `https://orbit.example.com`) |
| `git_url_prefix` | string | Base for clone URL; clone = `{git_url_prefix}{owner}/{repo}.git` |
| `auth` | string | `"bearer"` |

---

## Health

**GET /health**

Liveness/readiness. No auth.

**Response**: 200 OK (body optional).

---

## Authentication

### Register

**POST /auth/register**

**Body** (JSON): `{ "username", "email", "password" }`

**Response**: 201 + user or 400/409.

### Login

**POST /auth/login**

**Body** (JSON): `{ "login": "username or email", "password": "..." }`

**Response** (JSON): `{ "token", "user" }`. Use `token` as Bearer for API and Git HTTP.

### Personal access tokens

- **POST /auth/tokens** — Create PAT. Body: `{ "name", "expires_in_days"?: number }`. Response: `{ "token", "id", "name", "expires_at" }`. `token` shown only once.
- **GET /auth/tokens** — List PATs (no raw token). Response: array of `{ "id", "name", "created_at", "expires_at" }`.
- **DELETE /auth/tokens/{id}** — Revoke PAT.

---

## Users

- **GET /users/me** — Current user (auth required). Response: user object.
- **PATCH /users/me** — Update profile (auth required).
- **GET /users/{username}/repos** — List repos for a user. Optional auth; returns all for self, public only for others. Query: `limit`, `offset`.

---

## Repositories

| Method | Path | Description |
|--------|------|-------------|
| GET | /repos | List repos accessible to current user (auth). Query: `limit`, `offset`. |
| POST | /repos | Create repo (auth). Body: `{ "name", "description?", "visibility?" }`. |
| GET | /repos/{owner}/{repo} | Get repo metadata. Optional auth. |
| PATCH | /repos/{owner}/{repo} | Update repo (admin/owner). Body: `{ "name?", "description?" }`. |
| DELETE | /repos/{owner}/{repo} | Soft-delete repo (admin/owner). |
| POST | /repos/{owner}/{repo}/archive | Archive repo (admin/owner). |

**Repo response**: `id`, `owner_id`, `name`, `slug`, `description`, `visibility` (`"public"` \| `"private"`), `default_branch`, `archived`, `created_at`, `updated_at`.

---

## Branches

| Method | Path | Description |
|--------|------|-------------|
| GET | /repos/{owner}/{repo}/branches | List branches. Optional auth. |
| POST | /repos/{owner}/{repo}/branches | Create branch (write). Body: `{ "name", "start_point" }`. |
| GET | /repos/{owner}/{repo}/branches/{*branch} | Get branch. Optional auth. |
| DELETE | /repos/{owner}/{repo}/branches/{*branch} | Delete branch (write). |

---

## Commits and tree

| Method | Path | Description |
|--------|------|-------------|
| GET | /repos/{owner}/{repo}/commits | List commits. Query: `ref` (branch/tag/SHA, default branch if omitted), `limit`, `offset`. |
| GET | /repos/{owner}/{repo}/commits/{sha} | Get commit. |
| GET | /repos/{owner}/{repo}/commits/{sha}/diff | Get commit diff. |
| GET | /repos/{owner}/{repo}/tree/{ref}/{*path} | Browse tree. Empty path = root. |
| GET | /repos/{owner}/{repo}/blob/{ref}/{*path} | Get file content. |

---

## Tags

| Method | Path | Description |
|--------|------|-------------|
| GET | /repos/{owner}/{repo}/tags | List tags (name + target SHA). Query: `limit`, `offset`. Optional auth. |

**Tag response**: `name`, `target` (SHA), `peeled` (SHA for annotated tag, if any).

---

## Collaborators

| Method | Path | Description |
|--------|------|-------------|
| GET | /repos/{owner}/{repo}/collaborators | List collaborators (admin/owner). |
| PUT | /repos/{owner}/{repo}/collaborators/{username} | Add or update role. Body: `{ "role": "reader" \| "writer" \| "owner" }`. |
| DELETE | /repos/{owner}/{repo}/collaborators/{username} | Remove collaborator. |

---

## Pull requests

Pull request routes use the PR's UUID `id` in the path (returned in list/get responses). The `number` field remains in the response body for display (e.g. "PR #5").

| Method | Path | Description |
|--------|------|-------------|
| GET | /repos/{owner}/{repo}/pulls | List PRs. Query: `status`, `author_id`, `limit`, `offset`. |
| POST | /repos/{owner}/{repo}/pulls | Create PR (write). Body: `{ "source_branch", "target_branch", "title", "description?" }`. |
| GET | /repos/{owner}/{repo}/pulls/{id} | Get PR. `id` = UUID. |
| PATCH | /repos/{owner}/{repo}/pulls/{id} | Update title/description (author or write). Body: `{ "title?", "description?" }`. |
| POST | /repos/{owner}/{repo}/pulls/{id}/close | Close PR. |
| POST | /repos/{owner}/{repo}/pulls/{id}/reopen | Reopen PR. |
| GET | /repos/{owner}/{repo}/pulls/{id}/diff | Get PR diff (text). |
| GET | /repos/{owner}/{repo}/pulls/{id}/mergeability | Mergeability state. |
| GET | /repos/{owner}/{repo}/pulls/{id}/conflicts | Conflict check (files). |
| POST | /repos/{owner}/{repo}/pulls/{id}/merge | Merge PR (write). Body: `{ "strategy": "merge" \| "squash", "commit_message?" }`. |

---

## Repo events

**GET /repos/{owner}/{repo}/events**

Audit events for the repo. Query: `event_type`, `since`, `until`, `limit`, `offset`. Auth: read.

---

## Admin

All under `/admin/*`. Require admin authentication.

- **GET /admin/users** — List users. Query: `limit`, `offset`, `username` (prefix).
- **GET /admin/users/{id}** — Get user.
- **POST /admin/users/{id}/disable** — Disable user.
- **POST /admin/users/{id}/enable** — Enable user.
- **GET /admin/repos** — List repos. Query: `limit`, `offset`, `search`.
- **GET /admin/repos/{id}** — Get repo + `storage_exists`.
- **POST /admin/repos/{id}/archive** — Archive repo.
- **GET /admin/jobs** — List jobs. Query: `limit`, `offset`, `status`.
- **GET /admin/jobs/failed** — List failed jobs.
- **POST /admin/jobs/{id}/retry** — Retry job.
- **GET /admin/events** — Audit events. Query: `actor_id`, `repo_id`, `event_type`, `since`, `until`, `limit`, `offset`.

---

## Git HTTP transport

For standard Git clients (clone, fetch, push). Paths use `{repo}` **including** `.git` (e.g. `my-repo.git`).

- **GET /{owner}/{repo}/info/refs?service=git-upload-pack** — Ref advertisement for fetch/clone. Public repos: anonymous OK; private: auth + read.
- **GET /{owner}/{repo}/info/refs?service=git-receive-pack** — Ref advertisement for push. Auth + write required.
- **POST /{owner}/{repo}/git-upload-pack** — Pack negotiation (fetch/clone). Same auth as info/refs.
- **POST /{owner}/{repo}/git-receive-pack** — Push. Auth + write; rejects archived repos.

Use `Authorization: Bearer <token>` for private repos and push.

**Clone URL**: `https://<host>/<owner>/<repo>.git` (e.g. `https://orbit.example.com/alice/my-project.git`).

---

## Versioned API

REST routes are also mounted under **/v1** so clients can rely on a stable prefix:

- **GET /v1** — Same discovery JSON as **GET /api** (with `base_url` reflecting server).
- All REST routes above are available as **/v1/...** (e.g. **GET /v1/repos**, **GET /v1/repos/{owner}/{repo}/tags**).

Git HTTP and **GET /health** remain at the root (no `/v1`).

---

## Errors

- **400** Bad Request — Invalid input.
- **401** Unauthorized — Missing or invalid token.
- **403** Forbidden — Insufficient permission or repo archived.
- **404** Not Found — Resource missing (or private repo existence hidden).
- **409** Conflict — e.g. merge conflicts.
- **422** Unprocessable — e.g. PR not open, branch missing.
- **500** Internal Server Error.

Responses may include a JSON body with `message` or structured fields. Request ID: **x-request-id** response header.

---

## Rate limiting

When enabled (in-memory or Redis): auth (10/min), token create (20/min), repo create / repo write / admin mutations / git push (30/min per IP). Responses may include rate-limit headers; 429 when exceeded.
