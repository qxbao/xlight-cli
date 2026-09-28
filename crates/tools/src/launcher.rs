// SPDX-License-Identifier: GPL-3.0-only

//! `ProcessLauncher` (INV-1, PATTERNS.md §8): the **only** place in the whole workspace allowed
//! to spawn a child process. `clippy.toml` disallows `Command::new` everywhere else; this module
//! carries the one `#[allow(clippy::disallowed_methods)]`.
//!
//! Every spawned process gets its own process group (`tokio::process::Command::process_group`,
//! stable-safe std API — no `unsafe` needed, honoring `[workspace.lints.rust] unsafe_code =
//! "forbid"`); on timeout or cancellation, [`SpawnedProcess::pipe_into`] kills the **whole group**
//! via `rustix::process::kill_process_group` (also a safe call — sending a signal has no memory-
//! safety implications) so a shell pipeline's children die too, not just the immediate child.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::spool::OutputSpool;

/// Provider CLIs that must never be spawned (INV-1) — checked for `SpawnPurpose::{Mcp, Hook,
/// Git}` (defense in depth; the *primary* enforcement is simply that no code path in `runtime`
/// ever passes one of these as `program`, since core never imports a `provider-*` crate, CODEBASE
/// §3).
pub const PROVIDER_CLI_BLOCKLIST: &[&str] = &["codex", "claude", "agy", "antigravity"];

/// Why a process is being spawned. Only `Mcp`/`Hook`/`Git` are checked against
/// [`PROVIDER_CLI_BLOCKLIST`] (docs/PLAN.md §7.4) — a `ShellTool` command is opaque (`bash -lc
/// "<command>"`; the blocklist can't meaningfully filter arbitrary shell text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpawnPurpose {
    ShellTool,
    Mcp,
    Hook,
    Git,
}

/// How the child's environment is built. `Scrubbed` is the **only** variant (docs/PLAN.md §7.4):
/// there is no "inherit everything" API for MCP/hooks/shell.
#[derive(Debug, Clone)]
pub enum EnvPolicy {
    /// Starts from an empty environment and adds back only `passthrough` (each looked up from
    /// this process's own env at spawn time).
    Scrubbed { passthrough: Vec<String> },
}

impl EnvPolicy {
    /// No passthrough at all — the strictest policy, appropriate for MCP/hook spawns unless the
    /// user has explicitly configured otherwise.
    pub fn scrubbed() -> Self {
        Self::Scrubbed {
            passthrough: Vec::new(),
        }
    }
}

/// One spawn request.
#[derive(Debug)]
pub struct SpawnSpec {
    pub purpose: SpawnPurpose,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: EnvPolicy,
    /// `None` means no timeout (still killable via `cancel`).
    pub timeout: Option<Duration>,
    pub cancel: CancellationToken,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error(
        "{program:?} is not allowed as a {purpose:?} spawn target (provider-CLI blocklist, INV-1)"
    )]
    ForbiddenProgram {
        program: String,
        purpose: SpawnPurpose,
    },

    #[error("failed to spawn {program:?}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("process timed out after {0:?}")]
    Timeout(Duration),

    #[error("cancelled")]
    Cancelled,

    #[error("io error while streaming process output: {0}")]
    Io(#[from] std::io::Error),
}

fn basename(program: &str) -> &str {
    std::path::Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
}

fn is_forbidden(purpose: SpawnPurpose, program: &str) -> bool {
    matches!(
        purpose,
        SpawnPurpose::Mcp | SpawnPurpose::Hook | SpawnPurpose::Git
    ) && PROVIDER_CLI_BLOCKLIST.contains(&basename(program))
}

/// The single process-spawn point (PATTERNS.md §8). Cheap to construct; stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessLauncher;

impl ProcessLauncher {
    pub fn new() -> Self {
        Self
    }

    /// Spawns `spec`. Rejects a blocklisted program for `Mcp`/`Hook`/`Git` purposes before ever
    /// touching `Command` (`SpawnError::ForbiddenProgram`, no process created).
    pub async fn spawn(&self, spec: SpawnSpec) -> Result<SpawnedProcess, SpawnError> {
        if is_forbidden(spec.purpose, &spec.program) {
            return Err(SpawnError::ForbiddenProgram {
                program: spec.program.clone(),
                purpose: spec.purpose,
            });
        }
        let mut command = build_command(&spec);
        let mut child = command.spawn().map_err(|source| SpawnError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Ok(SpawnedProcess {
            child,
            stdout,
            stderr,
            timeout: spec.timeout,
            cancel: spec.cancel,
        })
    }
}

#[allow(clippy::disallowed_methods)] // PATTERNS.md §1: the only file allowed to call Command::new
fn build_command(spec: &SpawnSpec) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    match &spec.env {
        EnvPolicy::Scrubbed { passthrough } => {
            command.env_clear();
            for key in passthrough {
                if let Ok(value) = std::env::var(key) {
                    command.env(key, value);
                }
            }
        }
    }

    #[cfg(unix)]
    {
        // New process group (child PID == its own PGID): lets `SpawnedProcess::pipe_into` kill
        // the whole group, not just the immediate child, on timeout/cancel.
        command.process_group(0);
    }

    command
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
        // Best-effort: the process may have already exited between the timeout firing and this
        // call, which rustix reports as an error we don't need to propagate.
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

/// A running child process, produced by [`ProcessLauncher::spawn`].
#[derive(Debug)]
pub struct SpawnedProcess {
    child: tokio::process::Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    timeout: Option<Duration>,
    cancel: CancellationToken,
}

impl SpawnedProcess {
    /// The OS process id, if the child hasn't already exited.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Streams combined stdout+stderr into `spool` (PATTERNS.md §9) until the child exits, the
    /// timeout elapses, or `cancel` fires. On timeout/cancel, kills the whole process group
    /// (PATTERNS.md §8) and returns `Err` — the partial output already written to `spool`'s
    /// artifact file is not lost even though this call reports failure.
    pub async fn pipe_into(
        self,
        spool: &mut OutputSpool,
    ) -> Result<std::process::ExitStatus, SpawnError> {
        let Self {
            mut child,
            mut stdout,
            mut stderr,
            timeout,
            cancel,
        } = self;
        let pid = child.id();

        let read_both = async {
            let mut stdout_buf = [0_u8; 8192];
            let mut stderr_buf = [0_u8; 8192];
            let mut stdout_open = stdout.is_some();
            let mut stderr_open = stderr.is_some();
            while stdout_open || stderr_open {
                tokio::select! {
                    n = read_or_pending(&mut stdout, &mut stdout_buf), if stdout_open => {
                        match n {
                            Ok(0) | Err(_) => stdout_open = false,
                            Ok(n) => { let _ = spool.write_chunk(&stdout_buf[..n]).await; }
                        }
                    }
                    n = read_or_pending(&mut stderr, &mut stderr_buf), if stderr_open => {
                        match n {
                            Ok(0) | Err(_) => stderr_open = false,
                            Ok(n) => { let _ = spool.write_chunk(&stderr_buf[..n]).await; }
                        }
                    }
                }
            }
        };

        let timeout_fut = async {
            match timeout {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                if let Some(pid) = pid { kill_process_group(pid); }
                let _ = child.wait().await;
                Err(SpawnError::Cancelled)
            }
            _ = timeout_fut => {
                if let Some(pid) = pid { kill_process_group(pid); }
                let _ = child.wait().await;
                Err(SpawnError::Timeout(timeout.unwrap_or_default()))
            }
            _ = read_both => {
                child.wait().await.map_err(SpawnError::Io)
            }
        }
    }
}

/// Reads from `stream` if present; if `stream` is `None` (the child never had that fd, or it's
/// already been marked closed by the caller), never resolves — the caller only polls this branch
/// while its own `_open` flag is `true`, so a pending future here never actually blocks progress.
async fn read_or_pending<R: tokio::io::AsyncRead + Unpin>(
    stream: &mut Option<R>,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    match stream {
        Some(s) => s.read(buf).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use xlightcli_protocol::{SessionId, ToolCallId};

    use super::*;
    use crate::spool::SpoolLimits;

    fn spec(program: &str, args: &[&str], purpose: SpawnPurpose) -> SpawnSpec {
        SpawnSpec {
            purpose,
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: std::env::temp_dir(),
            env: EnvPolicy::scrubbed(),
            timeout: Some(Duration::from_secs(5)),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn blocklisted_program_is_rejected_for_mcp_purpose() {
        let launcher = ProcessLauncher::new();
        let err = launcher
            .spawn(spec("codex", &[], SpawnPurpose::Mcp))
            .await
            .unwrap_err();
        assert!(matches!(err, SpawnError::ForbiddenProgram { .. }));
    }

    #[tokio::test]
    async fn blocklisted_program_is_rejected_by_basename_even_with_a_path() {
        let launcher = ProcessLauncher::new();
        let err = launcher
            .spawn(spec("/usr/local/bin/claude", &[], SpawnPurpose::Hook))
            .await
            .unwrap_err();
        assert!(matches!(err, SpawnError::ForbiddenProgram { .. }));
    }

    #[tokio::test]
    async fn shell_tool_purpose_is_never_blocklist_checked() {
        // `program` here is deliberately the shell, not a blocklisted name — ShellTool purpose
        // just isn't checked at all (see the module doc).
        let launcher = ProcessLauncher::new();
        let result = launcher
            .spawn(spec("echo", &["hi"], SpawnPurpose::ShellTool))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn captures_stdout_into_the_spool() {
        let launcher = ProcessLauncher::new();
        let process = launcher
            .spawn(spec(
                "echo",
                &["hello from launcher"],
                SpawnPurpose::ShellTool,
            ))
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut spool = OutputSpool::create(
            dir.path(),
            SessionId::new(),
            ToolCallId::new("call-1"),
            SpoolLimits::default(),
        )
        .await
        .unwrap();
        let status = process.pipe_into(&mut spool).await.unwrap();
        assert!(status.success());
        let summary = spool.finish().await.unwrap();
        assert_eq!(summary.head_text().trim(), "hello from launcher");
    }

    #[tokio::test]
    async fn cancellation_stops_a_long_running_process() {
        let launcher = ProcessLauncher::new();
        let cancel = CancellationToken::new();
        let mut s = spec("sleep", &["30"], SpawnPurpose::ShellTool);
        s.cancel = cancel.clone();
        s.timeout = None;
        let process = launcher.spawn(s).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut spool = OutputSpool::create(
            dir.path(),
            SessionId::new(),
            ToolCallId::new("call-2"),
            SpoolLimits::default(),
        )
        .await
        .unwrap();

        cancel.cancel();
        let result = process.pipe_into(&mut spool).await;
        assert!(matches!(result, Err(SpawnError::Cancelled)));
    }

    #[tokio::test]
    async fn timeout_kills_the_process() {
        let launcher = ProcessLauncher::new();
        let mut s = spec("sleep", &["30"], SpawnPurpose::ShellTool);
        s.timeout = Some(Duration::from_millis(50));
        let process = launcher.spawn(s).await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mut spool = OutputSpool::create(
            dir.path(),
            SessionId::new(),
            ToolCallId::new("call-3"),
            SpoolLimits::default(),
        )
        .await
        .unwrap();

        let result = process.pipe_into(&mut spool).await;
        assert!(matches!(result, Err(SpawnError::Timeout(_))));
    }
}
