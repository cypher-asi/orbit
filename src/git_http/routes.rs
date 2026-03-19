use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use futures_util::StreamExt;

use serde::Deserialize;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

use crate::app_state::AppState;
use crate::auth::middleware::OptionalAuth;
use crate::errors::ApiError;
use crate::permissions::models::Permission;
use crate::permissions::service as permissions_service;
use crate::repos::models::Visibility;
use crate::storage;
use crate::storage::git::GitCommand;

use super::service::{pkt_flush, pkt_line, resolve_git_repo};

// ---------------------------------------------------------------------------
// Path / query extractors
// ---------------------------------------------------------------------------

/// Path parameters for `/{org_id}/{repo}` Git routes.
///
/// Note: `repo` includes the `.git` suffix (e.g. `my-repo.git`).
#[derive(Debug, Deserialize)]
pub struct GitRepoPath {
    pub org_id: uuid::Uuid,
    pub repo: String,
}

/// Query parameters for `info/refs`.
#[derive(Debug, Deserialize)]
pub struct InfoRefsQuery {
    pub service: Option<String>,
}

// ---------------------------------------------------------------------------
// GET /{org_id}/{repo}.git/info/refs
// ---------------------------------------------------------------------------

/// Handler for `GET /{org_id}/{repo}.git/info/refs?service=...`
///
/// Implements the Git Smart HTTP ref advertisement:
///
/// - `service=git-upload-pack` -- ref advertisement for clone/fetch.
///   Public repos allow unauthenticated access; private repos require auth
///   with read permission.
///
/// - `service=git-receive-pack` -- ref advertisement for push.
///   Always requires auth with write permission. Rejects archived repos.
///
/// Shells out to `git {service} --stateless-rpc --advertise-refs {repo_path}`
/// and prepends the pkt-line service announcement header.
pub async fn info_refs(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<GitRepoPath>,
    Query(query): Query<InfoRefsQuery>,
) -> Result<Response, ApiError> {
    // Validate the service query parameter.
    let service = query.service.as_deref().unwrap_or("");
    if service != "git-upload-pack" && service != "git-receive-pack" {
        return Err(ApiError::BadRequest(
            "invalid or missing service parameter".to_string(),
        ));
    }

    // Resolve the repository from the URL path.
    let (repo, disk_path) =
        resolve_git_repo(&state.db, state.git_storage_root.as_path(), path.org_id, &path.repo).await?;

    // Authorization checks based on the service type.
    let viewer_id = user.as_ref().map(|u| u.id);

    if service == "git-upload-pack" {
        // For upload-pack: public repos allow anonymous read; private repos
        // require auth + read permission.
        if repo.visibility == Visibility::Private {
            if viewer_id.is_none() {
                // Private repo, no auth -> 404 to hide existence.
                return Err(ApiError::NotFound("repository not found".to_string()));
            }
            permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read)
                .await?;
        }
        // Public repos: no auth check needed for read.
    } else {
        // git-receive-pack: always requires auth + write permission.
        if viewer_id.is_none() {
            return Err(ApiError::Forbidden("authentication required".to_string()));
        }

        // Check write permission (this also handles archived check).
        permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Write)
            .await?;

        // Explicitly check archived status for a clear error message.
        if repo.archived {
            return Err(ApiError::Forbidden("repository is archived".to_string()));
        }
    }

    // Verify disk path exists.
    if !disk_path.exists() {
        tracing::error!(
            repo_id = %repo.id,
            path = %disk_path.display(),
            "bare repo directory not found on disk"
        );
        return Err(ApiError::Internal(
            "repository storage not found".to_string(),
        ));
    }

    // Shell out to git to get the ref advertisement.
    // The bare service name without "git-" prefix is what git expects as
    // a subcommand (e.g. "upload-pack" not "git-upload-pack"), but since
    // service is already "git-upload-pack", we strip the "git-" prefix.
    let git_subcommand = service.strip_prefix("git-").unwrap_or(service);

    let git_cmd = GitCommand::new(disk_path.clone());
    let output = git_cmd
        .run(&[
            git_subcommand,
            "--stateless-rpc",
            "--advertise-refs",
            disk_path.to_str().unwrap_or("."),
        ])
        .await?;

    if !output.success() {
        tracing::error!(
            repo_id = %repo.id,
            stderr = %output.stderr,
            exit_code = output.exit_code,
            "git {} --advertise-refs failed", git_subcommand,
        );
        return Err(ApiError::Internal("git command failed".to_string()));
    }

    // Build the response body:
    //   1. pkt-line: "# service={service}\n"
    //   2. flush packet: "0000"
    //   3. raw git output
    let mut body = Vec::new();
    body.extend_from_slice(&pkt_line(&format!("# service={}\n", service)));
    body.extend_from_slice(&pkt_flush());
    body.extend_from_slice(&output.stdout);

    let content_type = format!("application/x-{}-advertisement", service);

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CACHE_CONTROL,
                "no-cache, max-age=0, must-revalidate".to_string(),
            ),
            (header::PRAGMA, "no-cache".to_string()),
            (header::EXPIRES, "Fri, 01 Jan 1980 00:00:00 GMT".to_string()),
        ],
        body,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// POST /{org_id}/{repo}.git/git-upload-pack
// ---------------------------------------------------------------------------

/// Handler for `POST /{org_id}/{repo}.git/git-upload-pack`.
///
/// Implements the Git Smart HTTP pack negotiation for clone/fetch:
///
/// - Public repos allow unauthenticated access.
/// - Private repos require auth with read permission.
///
/// Spawns `git upload-pack --stateless-rpc {repo_path}`, streams the
/// request body to the process stdin, and streams stdout back to the client.
///
/// Content-Type: `application/x-git-upload-pack-result`
pub async fn upload_pack(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<GitRepoPath>,
    body: Body,
) -> Result<Response, ApiError> {
    // Resolve the repository from the URL path.
    let (repo, disk_path) =
        resolve_git_repo(&state.db, state.git_storage_root.as_path(), path.org_id, &path.repo).await?;

    // Authorization: public repos allow anonymous read; private repos
    // require auth + read permission.
    let viewer_id = user.as_ref().map(|u| u.id);

    if repo.visibility == Visibility::Private {
        if viewer_id.is_none() {
            // Private repo, no auth -> 404 to hide existence.
            return Err(ApiError::NotFound("repository not found".to_string()));
        }
        permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Read)
            .await?;
    }

    // Verify disk path exists.
    if !disk_path.exists() {
        tracing::error!(
            repo_id = %repo.id,
            path = %disk_path.display(),
            "bare repo directory not found on disk"
        );
        return Err(ApiError::Internal(
            "repository storage not found".to_string(),
        ));
    }

    let disk_path_str = disk_path.to_str().unwrap_or(".").to_string();

    // Spawn the git upload-pack process.
    let mut child = tokio::process::Command::new("git")
        .args(["upload-pack", "--stateless-rpc", &disk_path_str])
        .env("GIT_DIR", &disk_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            tracing::error!(error = %e, "failed to spawn git upload-pack");
            ApiError::Internal("failed to execute git command".to_string())
        })?;

    // Take ownership of child stdin and pipe the request body into it.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ApiError::Internal("failed to open git process stdin".to_string()))?;

    // Spawn a task to write the request body to stdin.
    tokio::spawn(async move {
        let mut body_stream = body.into_data_stream();
        while let Some(chunk) = body_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // Drop stdin to close the pipe so git can finish.
        drop(stdin);
    });

    // Take ownership of stdout and stream it back as the response body.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::Internal("failed to open git process stdout".to_string()))?;

    let stream = tokio_util::io::ReaderStream::new(stdout);
    let response_body = Body::from_stream(stream);

    Ok((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/x-git-upload-pack-result".to_string(),
        )],
        response_body,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// POST /{org_id}/{repo}.git/git-receive-pack
// ---------------------------------------------------------------------------

/// Handler for `POST /{org_id}/{repo}.git/git-receive-pack`.
///
/// Implements the Git Smart HTTP push protocol:
///
/// - Always requires auth + write permission.
/// - Rejects pushes to archived repos.
///
/// Spawns `git receive-pack --stateless-rpc {repo_path}`, streams the
/// request body to the process stdin, and streams stdout back to the client.
///
/// Content-Type: `application/x-git-receive-pack-result`
///
/// After a successful receive, emits a `push.received` audit event.
pub async fn receive_pack(
    OptionalAuth(user): OptionalAuth,
    State(state): State<AppState>,
    Path(path): Path<GitRepoPath>,
    body: Body,
) -> Result<Response, ApiError> {
    // Resolve the repository from the URL path.
    let (repo, disk_path) =
        resolve_git_repo(&state.db, state.git_storage_root.as_path(), path.org_id, &path.repo).await?;

    // Authorization: always requires auth + write permission.
    let viewer_id = user.as_ref().map(|u| u.id);

    if viewer_id.is_none() {
        return Err(ApiError::Forbidden("authentication required".to_string()));
    }

    permissions_service::check_repo_access(&state.db, viewer_id, repo.id, Permission::Write)
        .await?;

    // Explicitly check archived status for a clear error message.
    if repo.archived {
        return Err(ApiError::Forbidden("repository is archived".to_string()));
    }

    // Verify disk path exists.
    if !disk_path.exists() {
        tracing::error!(
            repo_id = %repo.id,
            path = %disk_path.display(),
            "bare repo directory not found on disk"
        );
        return Err(ApiError::Internal(
            "repository storage not found".to_string(),
        ));
    }

    let disk_path_str = disk_path.to_str().unwrap_or(".").to_string();

    // Spawn the git receive-pack process.
    let mut child = tokio::process::Command::new("git")
        .args(["receive-pack", "--stateless-rpc", &disk_path_str])
        .env("GIT_DIR", &disk_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            tracing::error!(error = %e, "failed to spawn git receive-pack");
            ApiError::Internal("failed to execute git command".to_string())
        })?;

    // Take ownership of child stdin and pipe the request body into it.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ApiError::Internal("failed to open git process stdin".to_string()))?;

    // Spawn a task to write the request body to stdin.
    tokio::spawn(async move {
        let mut body_stream = body.into_data_stream();
        while let Some(chunk) = body_stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        drop(stdin);
    });

    // Take ownership of stdout and stream it back as the response body.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::Internal("failed to open git process stdout".to_string()))?;

    let stream = tokio_util::io::ReaderStream::new(stdout);
    let response_body = Body::from_stream(stream);

    // Emit audit event for the push (fire-and-forget).
    if let Some(actor_id) = viewer_id {
        let repo_id = repo.id;
        let db = state.db.clone();
        tokio::spawn(async move {
            storage::emit_audit_event(&db, actor_id, "push.received", Some(repo_id), None, None)
                .await;
        });
    }

    Ok((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "application/x-git-receive-pack-result".to_string(),
        )],
        response_body,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build a Router for Git read-only endpoints (info/refs and upload-pack).
///
/// These are used for clone/fetch operations and do not need aggressive
/// rate limiting.
///
/// Mounts:
/// - `GET  /{org_id}/{repo}/info/refs`       -- ref advertisement
/// - `POST /{org_id}/{repo}/git-upload-pack`  -- clone/fetch pack exchange
pub fn git_read_routes() -> Router<AppState> {
    Router::new()
        .route("/{org_id}/{repo}/info/refs", get(info_refs))
        .route("/{org_id}/{repo}/git-upload-pack", post(upload_pack))
}

/// Build a Router for the Git receive-pack (push) endpoint.
///
/// Push operations are expensive (disk I/O, pack processing) and should be
/// rate-limited separately.
///
/// Mounts:
/// - `POST /{org_id}/{repo}/git-receive-pack` -- push pack receive
pub fn git_receive_routes() -> Router<AppState> {
    Router::new().route("/{org_id}/{repo}/git-receive-pack", post(receive_pack))
}
