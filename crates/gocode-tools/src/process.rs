//! Process execution boundary used by `run_command` and the Git tools.

use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Provider credential environment variables stripped from every spawned process by default.
///
/// Only the NVIDIA provider exists in the MVP; this list grows alongside future providers
/// without changing the tool contract.
pub const DEFAULT_CREDENTIAL_ENV_VARS: &[&str] = &["NVIDIA_API_KEY"];

/// Maximum bytes of `stdout` or `stderr` retained per stream before head/tail truncation.
pub const DEFAULT_OUTPUT_LIMIT: usize = 100_000;

/// Removes values that commonly appear in copied diagnostic output before that output is shown,
/// streamed, or retained. This deliberately favours redacting too much over exposing a secret.
#[must_use]
pub fn redact_secrets(output: &str) -> String {
    [
        "token=",
        "token:",
        "password=",
        "password:",
        "secret=",
        "secret:",
        "api_key=",
        "api_key:",
        "authorization: bearer ",
        "cookie:",
    ]
    .into_iter()
    .fold(output.to_string(), |value, marker| {
        redact_after(&value, marker)
    })
}

fn redact_after(value: &str, marker: &str) -> String {
    let lowercase = value.to_ascii_lowercase();
    let marker_lowercase = marker.to_ascii_lowercase();
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(found) = lowercase[cursor..].find(&marker_lowercase) {
        let start = cursor + found;
        let value_start = start + marker.len();
        let secret_start = value_start
            + value[value_start..]
                .chars()
                .take_while(|character| character.is_whitespace())
                .map(char::len_utf8)
                .sum::<usize>();
        result.push_str(&value[cursor..secret_start]);
        let value_end = value[secret_start..]
            .find(char::is_whitespace)
            .map_or(value.len(), |offset| secret_start + offset);
        result.push_str("[REDACTED]");
        cursor = value_end;
    }
    result.push_str(&value[cursor..]);
    result
}

/// A structured request to execute one external process.
#[derive(Debug, Clone)]
pub struct CommandRequest {
    /// Executable name or path.
    pub program: String,
    /// Positional arguments.
    pub args: Vec<String>,
    /// Working directory, already validated to be inside the workspace.
    pub cwd: PathBuf,
    /// When true, `program`/`args` are interpreted as an already-composed shell command line
    /// instead of a direct `execve`-style invocation.
    pub shell: bool,
    /// Maximum wall-clock duration before the process is terminated.
    pub timeout: Duration,
}

impl CommandRequest {
    /// Creates a program-plus-arguments request with the crate's default timeout.
    #[must_use]
    pub fn new(program: impl Into<String>, args: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            program: program.into(),
            args,
            cwd,
            shell: false,
            timeout: Duration::from_secs(120),
        }
    }
}

/// The stream a chunk of output was captured from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Observes process output as it is produced, independent of the final bounded result.
pub trait OutputSink: Send + Sync {
    /// Records one chunk of output. Implementations must not block or panic.
    fn on_chunk(&self, stream: ProcessStream, chunk: &str);
}

/// How a process's execution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// The process ran to completion (an `exit_code` is available).
    Completed,
    /// The process was terminated because it exceeded its timeout.
    TimedOut,
    /// The process was terminated due to caller cancellation.
    Cancelled,
}

/// Result of a process that was spawned and ran to completion, timeout, or cancellation.
#[derive(Debug, Clone)]
pub struct ProcessResult {
    /// Exit code, when the process ran to completion.
    pub exit_code: Option<i32>,
    /// Captured standard output, possibly truncated.
    pub stdout: String,
    /// Captured standard error, possibly truncated.
    pub stderr: String,
    /// Whether `stdout` omits part of the full output.
    pub stdout_truncated: bool,
    /// Whether `stderr` omits part of the full output.
    pub stderr_truncated: bool,
    /// Wall-clock execution duration.
    pub duration_ms: u64,
    /// How execution ended.
    pub outcome: ProcessOutcome,
}

/// Failure to even run a process (as opposed to the process itself failing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    /// The executable could not be spawned (missing binary, invalid cwd, permission denied).
    SpawnFailed(String),
}

/// Future returned by [`ProcessRunner::run`].
pub type RunFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProcessResult, ProcessError>> + Send + 'a>>;

/// External-process execution boundary, abstracted so tests can substitute a fake runner and
/// Windows-specific process-tree handling can evolve independently of the tool contract.
pub trait ProcessRunner: Send + Sync {
    /// Executes `request`, honoring its timeout and `cancellation`.
    fn run(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
        sink: Option<Arc<dyn OutputSink>>,
    ) -> RunFuture<'_>;
}

/// The default [`ProcessRunner`], backed by `tokio::process`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioProcessRunner;

impl ProcessRunner for TokioProcessRunner {
    fn run(
        &self,
        request: CommandRequest,
        cancellation: CancellationToken,
        sink: Option<Arc<dyn OutputSink>>,
    ) -> RunFuture<'_> {
        Box::pin(async move { run_process(request, cancellation, sink).await })
    }
}

async fn run_process(
    request: CommandRequest,
    cancellation: CancellationToken,
    sink: Option<Arc<dyn OutputSink>>,
) -> Result<ProcessResult, ProcessError> {
    let started = Instant::now();
    let mut command = build_command(&request);

    // No stdin is ever supplied to a tool-invoked command. Without this, the child inherits the
    // agent's own stdin; a command that unexpectedly turns interactive (e.g. a shell invoked
    // without its "run this and exit" flag) then blocks reading from it until the timeout, rather
    // than failing fast on EOF.
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ProcessError::SpawnFailed(error.to_string()))?;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let mut stdout_buffer = BoundedBuffer::new(DEFAULT_OUTPUT_LIMIT);
    let mut stderr_buffer = BoundedBuffer::new(DEFAULT_OUTPUT_LIMIT);
    let mut stdout_open = stdout_pipe.is_some();
    let mut stderr_open = stderr_pipe.is_some();
    let mut stdout_chunk = [0_u8; 4096];
    let mut stderr_chunk = [0_u8; 4096];

    let mut timed_out = false;
    let mut cancelled = false;

    let deadline = tokio::time::sleep(request.timeout);
    tokio::pin!(deadline);

    'io: loop {
        if !stdout_open && !stderr_open {
            break;
        }

        tokio::select! {
            biased;
            () = cancellation.cancelled(), if !cancelled => {
                cancelled = true;
                break 'io;
            }
            () = &mut deadline, if !timed_out => {
                timed_out = true;
                break 'io;
            }
            result = read_chunk(&mut stdout_pipe, &mut stdout_chunk), if stdout_open => {
                match result {
                    Some(text) => {
                        if let Some(sink) = &sink {
                            sink.on_chunk(ProcessStream::Stdout, &redact_secrets(&text));
                        }
                        stdout_buffer.push(&redact_secrets(&text));
                    }
                    None => stdout_open = false,
                }
            }
            result = read_chunk(&mut stderr_pipe, &mut stderr_chunk), if stderr_open => {
                match result {
                    Some(text) => {
                        if let Some(sink) = &sink {
                            sink.on_chunk(ProcessStream::Stderr, &redact_secrets(&text));
                        }
                        stderr_buffer.push(&redact_secrets(&text));
                    }
                    None => stderr_open = false,
                }
            }
        }
    }

    let exit_code = if timed_out || cancelled {
        let _ = child.start_kill();
        let _ = child.wait().await;
        None
    } else {
        drain_remaining(
            &mut stdout_pipe,
            &mut stdout_buffer,
            sink.as_ref(),
            ProcessStream::Stdout,
        )
        .await;
        drain_remaining(
            &mut stderr_pipe,
            &mut stderr_buffer,
            sink.as_ref(),
            ProcessStream::Stderr,
        )
        .await;
        child.wait().await.ok().and_then(|status| status.code())
    };

    let (stdout, stdout_truncated) = stdout_buffer.finish();
    let (stderr, stderr_truncated) = stderr_buffer.finish();
    let outcome = if cancelled {
        ProcessOutcome::Cancelled
    } else if timed_out {
        ProcessOutcome::TimedOut
    } else {
        ProcessOutcome::Completed
    };

    Ok(ProcessResult {
        exit_code,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        outcome,
    })
}

async fn read_chunk(
    pipe: &mut Option<impl tokio::io::AsyncRead + Unpin>,
    buffer: &mut [u8],
) -> Option<String> {
    let reader = pipe.as_mut()?;
    match reader.read(buffer).await {
        Ok(0) | Err(_) => None,
        Ok(count) => Some(String::from_utf8_lossy(&buffer[..count]).into_owned()),
    }
}

async fn drain_remaining(
    pipe: &mut Option<impl tokio::io::AsyncRead + Unpin>,
    buffer: &mut BoundedBuffer,
    sink: Option<&Arc<dyn OutputSink>>,
    stream: ProcessStream,
) {
    let Some(reader) = pipe.as_mut() else { return };
    let mut remaining = String::new();
    if reader.read_to_string(&mut remaining).await.is_ok() && !remaining.is_empty() {
        if let Some(sink) = sink {
            sink.on_chunk(stream, &redact_secrets(&remaining));
        }
        buffer.push(&redact_secrets(&remaining));
    }
}

fn build_command(request: &CommandRequest) -> tokio::process::Command {
    let mut command = if request.shell {
        #[cfg(windows)]
        {
            let mut command = tokio::process::Command::new("cmd");
            command.arg("/C").arg(&request.program);
            command.args(&request.args);
            command
        }
        #[cfg(not(windows))]
        {
            let mut command = tokio::process::Command::new("/bin/sh");
            command.arg("-c").arg(&request.program);
            command.args(&request.args);
            command
        }
    } else {
        let mut command = tokio::process::Command::new(&request.program);
        command.args(&request.args);
        command
    };

    command.current_dir(&request.cwd);
    command.kill_on_drop(true);
    for name in DEFAULT_CREDENTIAL_ENV_VARS {
        command.env_remove(name);
    }
    command
}

/// Retains the first and last `limit` bytes of pushed content, marking the gap as truncated.
struct BoundedBuffer {
    limit: usize,
    head: String,
    tail: String,
    truncated: bool,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            head: String::new(),
            tail: String::new(),
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &str) {
        if self.head.len() < self.limit {
            let remaining = self.limit - self.head.len();
            if chunk.len() <= remaining {
                self.head.push_str(chunk);
                return;
            }
            let mut split = remaining;
            while split > 0 && !chunk.is_char_boundary(split) {
                split -= 1;
            }
            self.head.push_str(&chunk[..split]);
            self.truncated = true;
            self.push_tail(&chunk[split..]);
            return;
        }
        self.truncated = true;
        self.push_tail(chunk);
    }

    fn push_tail(&mut self, chunk: &str) {
        self.tail.push_str(chunk);
        if self.tail.len() > self.limit {
            let excess = self.tail.len() - self.limit;
            let mut cut = excess;
            while cut < self.tail.len() && !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.tail.drain(..cut);
        }
    }

    fn finish(self) -> (String, bool) {
        if self.truncated {
            (
                format!("{}\n[... output truncated ...]\n{}", self.head, self.tail),
                true,
            )
        } else {
            (self.head, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommandRequest, DEFAULT_CREDENTIAL_ENV_VARS, ProcessOutcome, ProcessRunner,
        TokioProcessRunner,
    };
    use std::{path::PathBuf, time::Duration};
    use tokio_util::sync::CancellationToken;

    fn cwd() -> PathBuf {
        std::env::current_dir().expect("current directory should resolve")
    }

    #[tokio::test]
    async fn successful_process_captures_stdout_and_exit_code() {
        let runner = TokioProcessRunner;
        #[cfg(windows)]
        let request = CommandRequest::new("cmd", vec!["/C".into(), "echo hello".into()], cwd());
        #[cfg(not(windows))]
        let request = CommandRequest::new("echo", vec!["hello".into()], cwd());

        let result = runner
            .run(request, CancellationToken::new(), None)
            .await
            .expect("process should spawn");

        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello"));
        assert_eq!(result.outcome, ProcessOutcome::Completed);
    }

    #[tokio::test]
    async fn missing_executable_reports_spawn_failure() {
        let runner = TokioProcessRunner;
        let request = CommandRequest::new("definitely-not-a-real-executable-xyz", vec![], cwd());

        let error = runner.run(request, CancellationToken::new(), None).await;
        assert!(error.is_err());
    }

    #[tokio::test]
    async fn timeout_terminates_a_long_running_process() {
        let runner = TokioProcessRunner;
        #[cfg(windows)]
        let mut request = CommandRequest::new(
            "cmd",
            vec![
                "/C".into(),
                "ping".into(),
                "-n".into(),
                "20".into(),
                "127.0.0.1".into(),
            ],
            cwd(),
        );
        #[cfg(not(windows))]
        let mut request = CommandRequest::new("sleep", vec!["5".into()], cwd());
        request.timeout = Duration::from_millis(200);

        let result = runner
            .run(request, CancellationToken::new(), None)
            .await
            .expect("process should spawn");

        assert_eq!(result.outcome, ProcessOutcome::TimedOut);
        assert_eq!(result.exit_code, None);
    }

    #[tokio::test]
    async fn cancellation_terminates_a_running_process() {
        let runner = TokioProcessRunner;
        #[cfg(windows)]
        let request = CommandRequest::new(
            "cmd",
            vec![
                "/C".into(),
                "ping".into(),
                "-n".into(),
                "20".into(),
                "127.0.0.1".into(),
            ],
            cwd(),
        );
        #[cfg(not(windows))]
        let request = CommandRequest::new("sleep", vec!["5".into()], cwd());

        let token = CancellationToken::new();
        let cancel_token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_token.cancel();
        });

        let result = runner
            .run(request, token, None)
            .await
            .expect("process should spawn");

        assert_eq!(result.outcome, ProcessOutcome::Cancelled);
        assert_eq!(result.exit_code, None);
    }

    #[test]
    fn credential_environment_variables_are_explicitly_removed_from_the_child_command() {
        let request = CommandRequest::new("echo", vec!["hi".into()], cwd());
        let command = super::build_command(&request);

        let removed_nvidia_key = command
            .as_std()
            .get_envs()
            .any(|(key, value)| key == "NVIDIA_API_KEY" && value.is_none());

        assert!(removed_nvidia_key);
        assert_eq!(DEFAULT_CREDENTIAL_ENV_VARS, &["NVIDIA_API_KEY"]);
    }

    #[test]
    fn redacts_common_secrets_from_process_output() {
        let output = "token=abc123 password: hunter2 Authorization: Bearer xyz";
        let redacted = super::redact_secrets(output);
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("hunter2"));
        assert!(!redacted.contains("xyz"));
        assert!(redacted.contains("[REDACTED]"));
    }
}
