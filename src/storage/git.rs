use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::errors::ApiError;

/// Default timeout for Git operations (300 seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Structured output from a completed Git command.
#[derive(Debug)]
pub struct CommandOutput {
    /// Raw stdout bytes (Git data is often binary).
    pub stdout: Vec<u8>,
    /// Stderr captured as a UTF-8 string (Git diagnostics are text).
    pub stderr: String,
    /// Process exit code.
    pub exit_code: i32,
}

impl CommandOutput {
    /// Returns `true` if the command exited with status code 0.
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Helper for executing Git CLI commands against a specific repository.
///
/// All commands are executed with:
/// - `GIT_DIR` set to the repository path
/// - `GIT_TERMINAL_PROMPT=0` to prevent interactive prompts
/// - A configurable timeout (default 300s)
#[derive(Debug, Clone)]
pub struct GitCommand {
    repo_path: PathBuf,
    timeout: Duration,
}

impl GitCommand {
    /// Create a new `GitCommand` targeting the given bare repository path.
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            repo_path,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Override the default timeout for Git operations.
    #[allow(dead_code)]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run a Git command, capturing stdout and stderr.
    ///
    /// `args` are passed directly to the `git` binary. For example,
    /// `&["rev-parse", "HEAD"]` executes `git rev-parse HEAD`.
    pub async fn run(&self, args: &[&str]) -> Result<CommandOutput, ApiError> {
        let child = Command::new("git")
            .args(args)
            .env("GIT_DIR", &self.repo_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                tracing::error!(error = %e, args = ?args, "failed to spawn git process");
                ApiError::Internal("failed to execute git command".to_string())
            })?;

        let result = tokio::time::timeout(self.timeout, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                Ok(CommandOutput {
                    stdout: output.stdout,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code,
                })
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, args = ?args, "git process I/O error");
                Err(ApiError::Internal("git command failed".to_string()))
            }
            Err(_) => {
                tracing::error!(
                    args = ?args,
                    timeout_secs = self.timeout.as_secs(),
                    "git command timed out"
                );
                // child is killed on drop due to kill_on_drop(true)
                Err(ApiError::Internal("git command timed out".to_string()))
            }
        }
    }

    /// Return a reference to the repository path this command targets.
    #[allow(dead_code)]
    pub fn repo_path(&self) -> &PathBuf {
        &self.repo_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_output_success_returns_true_on_zero() {
        let output = CommandOutput {
            stdout: vec![],
            stderr: String::new(),
            exit_code: 0,
        };
        assert!(output.success());
    }

    #[tokio::test]
    async fn command_output_success_returns_false_on_nonzero() {
        let output = CommandOutput {
            stdout: vec![],
            stderr: String::new(),
            exit_code: 1,
        };
        assert!(!output.success());
    }

    #[tokio::test]
    async fn run_captures_stdout_and_exit_code() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let repo_path = tmp.path().join("test.git");

        // Initialize a bare repo first.
        let init_output = tokio::process::Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&repo_path)
            .output()
            .await
            .expect("failed to run git init");
        assert!(init_output.status.success());

        let cmd = GitCommand::new(repo_path);
        let output = cmd.run(&["rev-parse", "--git-dir"]).await.unwrap();
        assert!(output.success());
        // stdout should contain the git dir path
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout_str.trim().is_empty());
    }

    #[tokio::test]
    async fn run_captures_failure() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let repo_path = tmp.path().join("test.git");

        // Initialize a bare repo first.
        let init_output = tokio::process::Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&repo_path)
            .output()
            .await
            .expect("failed to run git init");
        assert!(init_output.status.success());

        let cmd = GitCommand::new(repo_path);
        // rev-parse on a nonexistent ref should fail
        let output = cmd.run(&["rev-parse", "HEAD"]).await.unwrap();
        assert!(!output.success());
        assert!(!output.stderr.is_empty());
    }

    #[tokio::test]
    async fn timeout_is_configurable() {
        let cmd =
            GitCommand::new(PathBuf::from("/tmp/test.git")).with_timeout(Duration::from_secs(60));
        assert_eq!(cmd.timeout, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn git_terminal_prompt_is_disabled() {
        // We test indirectly: the command should never hang waiting for input.
        // Running against a non-existent repo should fail quickly, not block.
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let repo_path = tmp.path().join("nonexistent.git");

        let cmd = GitCommand::new(repo_path).with_timeout(Duration::from_secs(5));
        let result = cmd.run(&["status"]).await;
        // Should complete (not hang) even though repo doesn't exist.
        // The command itself may error, but it should not timeout.
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.success());
    }
}
