//! Pipe command runner (§5.4, §7.1, §11.4): one supervised process group per run.
//!
//! The runner only reports OS facts (spawn, exit, signals, cleanup). The actor decides the
//! run outcome. stdout/stderr are drained continuously into the run log; for LPP/1 plugins
//! stdout carries frames instead (see [`crate::plugin_runner`]).

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::ids::{RunId, ViewId};
use lyra_protocol::lpp::PluginEvent;
use lyra_protocol::manifest::{StopSignal, TimeoutPolicy};
use lyra_protocol::run::{CleanupState, ExitInfo, LogRecord, LogStream};
use lyra_protocol::time::Timestamp;
use lyra_protocol::view::LogLevel;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

use crate::env::ChildEnv;
use crate::logs::{FLUSH_EVERY, SharedLog};
use crate::plugin_runner::{FrameOut, FrameReader, Protocol};

const CLEANUP_TIMEOUT: Duration = Duration::from_millis(lyra_protocol::limits::CLEANUP_TIMEOUT_MS);
/// After the main process exits, keep draining pipes held by leftover group members this long.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);
const FAR: Duration = Duration::from_secs(86_400 * 365);

pub struct CommandSpec {
    pub run_id: RunId,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Where `cleanup` runs; the action cwd (plugins themselves run in the plugin dir).
    pub cleanup_cwd: PathBuf,
    pub env: ChildEnv,
    pub timeout: TimeoutPolicy,
    pub stop_signal: StopSignal,
    pub grace: Duration,
    pub cleanup: Option<Vec<String>>,
    /// Temporary input/config files removed after the child and cleanup finish.
    pub temp_files: Vec<PathBuf>,
    /// Set for LPP/1 plugin runs: stdin gets the invocation, stdout carries frames.
    pub protocol: Option<Protocol>,
}

/// Why the actor asks a run to stop; forwarded to cleanup as `LYRA_STOP_REASON`.
#[derive(Debug, Clone, Copy)]
pub enum StopKind {
    Cancelled,
    SessionClosed,
    /// The host stops the run itself (protocol error).
    Failed,
}

#[derive(Debug)]
pub enum RunnerEvent {
    Spawned,
    TimedOut,
    Logs {
        records: Vec<LogRecord>,
    },
    /// A validated non-log LPP frame, in receive order.
    Frame(Box<PluginEvent>),
    /// The first LPP protocol error of this run; later stdout is discarded.
    ProtocolError {
        error: ErrorInfo,
        view_hint: Option<ViewId>,
    },
    Finished(Box<FinishedRun>),
}

#[derive(Debug)]
pub struct FinishedRun {
    pub exit: Option<ExitInfo>,
    pub spawn_error: Option<ErrorInfo>,
    pub cleanup: CleanupState,
}

pub type EventSink = mpsc::Sender<(RunId, RunnerEvent)>;

pub fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        3 => "SIGQUIT".into(),
        6 => "SIGABRT".into(),
        9 => "SIGKILL".into(),
        13 => "SIGPIPE".into(),
        14 => "SIGALRM".into(),
        15 => "SIGTERM".into(),
        n => format!("SIG{n}"),
    }
}

pub fn exit_info(status: ExitStatus) -> ExitInfo {
    ExitInfo {
        code: status.code(),
        signal: status.signal().map(signal_name),
    }
}

fn signal_group(pid: u32, sig: Signal) {
    if let Some(p) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(p, sig);
    }
}

fn command(argv: &[String], cwd: &PathBuf, env: &ChildEnv, stdin: Stdio) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(&env.0)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(false);
    cmd
}

struct Pipes {
    out: Option<ChildStdout>,
    err: Option<ChildStderr>,
}

async fn read_opt<R: AsyncReadExt + Unpin>(r: &mut Option<R>, buf: &mut [u8]) -> Option<usize> {
    match r {
        Some(reader) => reader.read(buf).await.ok(),
        None => std::future::pending().await,
    }
}

struct Batcher {
    run_id: RunId,
    log: SharedLog,
    events: EventSink,
    pending: Vec<LogRecord>,
    /// LPP/1 frame reader; `None` means stdout is plain log output.
    frames: Option<FrameReader>,
    /// Frame events waiting for delivery. Delivery awaits the actor (backpressure).
    outbox: Vec<RunnerEvent>,
}

impl Batcher {
    fn stdout(&mut self, bytes: &[u8]) {
        match self.frames.as_mut() {
            None => self.push(LogStream::Stdout, bytes),
            Some(reader) => {
                let outs = reader.push(bytes);
                self.frame_outs(outs);
            }
        }
    }
    fn stdout_eof(&mut self) {
        if let Some(out) = self.frames.as_mut().and_then(FrameReader::finish) {
            self.frame_outs(vec![out]);
        }
    }
    fn partial_pending(&self) -> bool {
        self.frames
            .as_ref()
            .is_some_and(FrameReader::partial_pending)
    }
    fn partial_deadline(&self) -> Instant {
        self.frames
            .as_ref()
            .map_or(Instant::now() + FAR, FrameReader::partial_deadline)
    }
    fn partial_timeout(&mut self) {
        if let Some(out) = self.frames.as_mut().and_then(FrameReader::partial_timeout) {
            self.frame_outs(vec![out]);
        }
    }
    fn frame_outs(&mut self, outs: Vec<FrameOut>) {
        for out in outs {
            match out {
                FrameOut::Log {
                    level,
                    text,
                    fields,
                } => {
                    if let Ok(mut log) = self.log.lock() {
                        self.pending.extend(log.push_line_fields(
                            LogStream::Plugin,
                            level,
                            &text,
                            false,
                            Some(fields),
                        ));
                    }
                }
                FrameOut::Event(ev) => {
                    // Keep log records ordered before the frame that followed them.
                    self.send();
                    self.outbox.push(RunnerEvent::Frame(Box::new(ev)));
                }
                FrameOut::Error { error, view_hint } => {
                    self.note(&format!(
                        "protocol error [{}]: {}",
                        error.code, error.message
                    ));
                    self.send();
                    self.outbox
                        .push(RunnerEvent::ProtocolError { error, view_hint });
                }
            }
        }
    }
    /// Delivers frame events without dropping any; waits while the actor queue is full.
    async fn deliver(&mut self) {
        for ev in std::mem::take(&mut self.outbox) {
            let _ = self.events.send((self.run_id.clone(), ev)).await;
        }
    }
    fn push(&mut self, stream: LogStream, bytes: &[u8]) {
        if let Ok(mut log) = self.log.lock() {
            self.pending.extend(log.push_bytes(stream, bytes));
        }
        if self.pending.len() >= 256 {
            self.send();
        }
    }
    fn note(&mut self, text: &str) {
        if let Ok(mut log) = self.log.lock() {
            self.pending
                .extend(log.push_line(LogStream::Host, LogLevel::Info, text, false));
        }
    }
    fn tick(&mut self) {
        if let Ok(mut log) = self.log.lock()
            && log.flush_due()
        {
            log.flush();
        }
        self.send();
    }
    fn finish(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            self.pending.extend(log.finish_streams());
            log.flush();
        }
        self.send();
    }
    fn send(&mut self) {
        if !self.pending.is_empty() {
            let records = std::mem::take(&mut self.pending);
            // Live delivery is best effort; the log file and ring stay authoritative.
            let _ = self
                .events
                .try_send((self.run_id.clone(), RunnerEvent::Logs { records }));
        }
    }
}

/// Drives one child: returns the exit status when the main process ends.
async fn drive(
    child: &mut Child,
    pipes: &mut Pipes,
    batch: &mut Batcher,
    stop_rx: &mut mpsc::Receiver<StopKind>,
    timeout: Option<Duration>,
    signal: Signal,
    grace: Duration,
) -> (Option<ExitStatus>, bool, Option<StopKind>) {
    let pid = child.id().unwrap_or(0);
    let mut buf = vec![0u8; 64 * 1024];
    let mut ebuf = vec![0u8; 64 * 1024];
    let timeout_at = Instant::now() + timeout.unwrap_or(FAR);
    let mut kill_at = Instant::now() + FAR;
    let mut stopping = false;
    let mut timed_out = false;
    let mut stop_kind = None;
    let mut flush = tokio::time::interval(FLUSH_EVERY);
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.ok(),
            n = read_opt(&mut pipes.out, &mut buf) => {
                match n {
                    Some(n) if n > 0 => batch.stdout(&buf[..n]),
                    _ => {
                        pipes.out = None;
                        batch.stdout_eof();
                    }
                }
                batch.deliver().await;
            },
            n = read_opt(&mut pipes.err, &mut ebuf) => match n {
                Some(n) if n > 0 => batch.push(LogStream::Stderr, &ebuf[..n]),
                _ => pipes.err = None,
            },
            kind = stop_rx.recv(), if !stopping => {
                stop_kind = kind;
                stopping = true;
                batch.note(&format!("stopping: sending {} to the process group", if signal == Signal::INT { "SIGINT" } else { "SIGTERM" }));
                signal_group(pid, signal);
                kill_at = Instant::now() + grace;
            }
            _ = sleep_until(timeout_at), if !stopping => {
                stopping = true;
                timed_out = true;
                let _ = batch.events.try_send((batch.run_id.clone(), RunnerEvent::TimedOut));
                batch.note("timeout reached: stopping the process group");
                signal_group(pid, signal);
                kill_at = Instant::now() + grace;
            }
            _ = sleep_until(kill_at) => {
                batch.note("grace period over: sending SIGKILL to the process group");
                signal_group(pid, Signal::KILL);
                kill_at = Instant::now() + FAR;
            }
            _ = sleep_until(batch.partial_deadline()), if batch.partial_pending() => {
                batch.partial_timeout();
                batch.deliver().await;
            }
            _ = flush.tick() => batch.tick(),
        }
    };
    // Drain what leftover group members still write, then clear the group if we were stopping.
    let drain_until = Instant::now() + DRAIN_AFTER_EXIT;
    while pipes.out.is_some() || pipes.err.is_some() {
        tokio::select! {
            n = read_opt(&mut pipes.out, &mut buf) => {
                match n {
                    Some(n) if n > 0 => batch.stdout(&buf[..n]),
                    _ => {
                        pipes.out = None;
                        batch.stdout_eof();
                    }
                }
                batch.deliver().await;
            },
            n = read_opt(&mut pipes.err, &mut ebuf) => match n {
                Some(n) if n > 0 => batch.push(LogStream::Stderr, &ebuf[..n]),
                _ => pipes.err = None,
            },
            _ = sleep_until(drain_until) => {
                if stopping {
                    signal_group(pid, Signal::KILL);
                }
                break;
            }
        }
    }
    // A frame still open when the pipes are abandoned is truncated.
    if pipes.out.is_some() {
        batch.stdout_eof();
        batch.deliver().await;
    }
    batch.finish();
    (status, timed_out, stop_kind)
}

async fn run_cleanup(spec: &CommandSpec, reason: &str, batch: &mut Batcher) -> CleanupState {
    let Some(argv) = &spec.cleanup else {
        return CleanupState::NotNeeded;
    };
    let mut env = spec.env.clone();
    env.0.insert("LYRA_STOP_REASON".into(), reason.into());
    batch.note(&format!("cleanup: running {}", argv[0]));
    let mut child = match command(argv, &spec.cleanup_cwd, &env, Stdio::null()).spawn() {
        Ok(c) => c,
        Err(e) => {
            return CleanupState::Failed {
                ended_at: Timestamp::now(),
                error: ErrorInfo::new(
                    ErrorCode::EXECUTION_FAILED,
                    format!("cannot start cleanup: {e}"),
                ),
            };
        }
    };
    let mut pipes = Pipes {
        out: child.stdout.take(),
        err: child.stderr.take(),
    };
    let (_tx, mut never) = mpsc::channel::<StopKind>(1);
    let (status, timed_out, _) = drive(
        &mut child,
        &mut pipes,
        batch,
        &mut never,
        Some(CLEANUP_TIMEOUT),
        Signal::TERM,
        Duration::from_secs(1),
    )
    .await;
    let ended_at = Timestamp::now();
    match status {
        Some(s) if s.success() && !timed_out => CleanupState::Succeeded { ended_at },
        Some(s) => CleanupState::Failed {
            ended_at,
            error: ErrorInfo::new(
                if timed_out {
                    ErrorCode::TIMEOUT
                } else {
                    ErrorCode::EXECUTION_FAILED
                },
                match (timed_out, s.code(), s.signal()) {
                    (true, _, _) => "cleanup timed out after 10 s".to_owned(),
                    (_, Some(c), _) => format!("cleanup exited with status {c}"),
                    (_, None, Some(sig)) => format!("cleanup was ended by {}", signal_name(sig)),
                    _ => "cleanup ended abnormally".to_owned(),
                },
            ),
        },
        None => CleanupState::Unknown {
            message: "cleanup status could not be read".into(),
        },
    }
}

/// Supervises one run from spawn to final cleanup. Never panics on child failures.
pub async fn supervise(
    spec: CommandSpec,
    log: SharedLog,
    events: EventSink,
    mut stop_rx: mpsc::Receiver<StopKind>,
) {
    let mut batch = Batcher {
        run_id: spec.run_id.clone(),
        log,
        events: events.clone(),
        pending: Vec::new(),
        frames: spec.protocol.as_ref().map(|p| FrameReader::new(p.mode)),
        outbox: Vec::new(),
    };
    let signal = match spec.stop_signal {
        StopSignal::Term => Signal::TERM,
        StopSignal::Interrupt => Signal::INT,
    };
    let finish = |exit, spawn_error, cleanup| {
        RunnerEvent::Finished(Box::new(FinishedRun {
            exit,
            spawn_error,
            cleanup,
        }))
    };
    let stdin = if spec.protocol.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = match command(&spec.argv, &spec.cwd, &spec.env, stdin).spawn() {
        Ok(c) => c,
        Err(e) => {
            let err = ErrorInfo::new(
                ErrorCode::EXECUTION_FAILED,
                format!("cannot start `{}`: {e}", spec.argv[0]),
            );
            batch.note(&err.message);
            batch.finish();
            remove_temp(&spec.temp_files);
            let _ = events
                .send((
                    spec.run_id.clone(),
                    finish(None, Some(err), CleanupState::NotNeeded),
                ))
                .await;
            return;
        }
    };
    let _ = events
        .send((spec.run_id.clone(), RunnerEvent::Spawned))
        .await;
    // LPP/1: exactly one invocation line, then EOF. A writer task keeps a plugin that does
    // not read stdin from blocking the supervisor.
    if let (Some(p), Some(mut stdin)) = (&spec.protocol, child.stdin.take()) {
        let line = p.invocation_line.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(&line).await;
            let _ = stdin.shutdown().await;
        });
    }
    let mut pipes = Pipes {
        out: child.stdout.take(),
        err: child.stderr.take(),
    };
    let timeout = match spec.timeout {
        TimeoutPolicy::Unlimited => None,
        TimeoutPolicy::After(d) => Some(d),
    };
    let (status, timed_out, stop_kind) = drive(
        &mut child,
        &mut pipes,
        &mut batch,
        &mut stop_rx,
        timeout,
        signal,
        spec.grace,
    )
    .await;
    let exit = status.map(exit_info);
    let reason = match (timed_out, stop_kind, status) {
        (true, _, _) => "timed_out",
        (_, Some(StopKind::SessionClosed), _) => "session_closed",
        (_, Some(StopKind::Cancelled), _) => "cancelled",
        (_, Some(StopKind::Failed), _) => "failed",
        (_, None, Some(s)) if s.success() => "completed",
        _ => "failed",
    };
    // Cleanup output is plain log output even for plugins.
    batch.frames = None;
    let cleanup = run_cleanup(&spec, reason, &mut batch).await;
    batch.finish();
    remove_temp(&spec.temp_files);
    let _ = events
        .send((spec.run_id.clone(), finish(exit, None, cleanup)))
        .await;
}

fn remove_temp(files: &[PathBuf]) {
    for f in files {
        let _ = std::fs::remove_file(f);
    }
}
