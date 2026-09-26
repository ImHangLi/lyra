//! Run control commands (§5.2, §10.2): the CLI waits; the host never blocks on a task.

use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use lyra_client::{Client, ConnectOptions};
use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::ids::{ActionRef, RequestKey, RunId};
use lyra_protocol::ipc::*;
use lyra_protocol::limits::MAX_PUBLIC_REPLY_BYTES;
use lyra_protocol::manifest::TimeoutWire;
use lyra_protocol::reply::{PublicReply, ReplyContext};
use lyra_protocol::run::{Lifecycle, Outcome, RunRecord};
use serde_json::{Map, Value};

use super::ctx::{Ctx, block_on};
use super::inspect::lifecycle_text;
use crate::output::{self, Mode, invalid_argument};

const POLL: Duration = Duration::from_millis(100);

fn env() -> Result<ClientEnv, ErrorInfo> {
    ClientEnv::capture().map_err(|m| ErrorInfo::new(ErrorCode::INVALID_ARGUMENT, m))
}

/// Reads `--input FILE|-` as one strict JSON object.
pub fn read_input(input: Option<&str>) -> Result<Map<String, Value>, ErrorInfo> {
    let Some(src) = input else {
        return Ok(Map::new());
    };
    let bytes = if src == "-" {
        let mut b = Vec::new();
        std::io::stdin()
            .take(MAX_PUBLIC_REPLY_BYTES as u64 + 1)
            .read_to_end(&mut b)
            .map_err(|e| invalid_argument(e.to_string()))?;
        b
    } else {
        std::fs::read(Path::new(src))
            .map_err(|e| invalid_argument(format!("cannot read {src}: {e}")))?
    };
    match lyra_protocol::strict_json::parse(&bytes, MAX_PUBLIC_REPLY_BYTES) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(ErrorInfo::new(
            ErrorCode::SCHEMA_INVALID,
            "input must be a JSON object",
        )),
        Err(e) => Err(ErrorInfo::new(
            ErrorCode::SCHEMA_INVALID,
            format!("invalid input JSON: {e}"),
        )),
    }
}

fn parse_action(s: &str) -> Result<ActionRef, ErrorInfo> {
    s.parse()
        .map_err(|e| invalid_argument(format!("{e}: `{s}`")))
}

fn parse_key(k: Option<String>) -> Result<Option<RequestKey>, ErrorInfo> {
    k.map(RequestKey::parse)
        .transpose()
        .map_err(|e| invalid_argument(e.to_string()))
}

pub fn parse_target(s: &str) -> Result<RunTarget, ErrorInfo> {
    if s.starts_with("r_") {
        RunId::parse(s.to_owned())
            .map(|run_id| RunTarget::Run { run_id })
            .map_err(|e| invalid_argument(e.to_string()))
    } else {
        parse_action(s).map(|action_ref| RunTarget::Action { action_ref })
    }
}

/// `30s`, `30m`, `2h`, `1d`, or `none`.
pub fn parse_ttl(s: &str) -> Result<TimeoutWire, ErrorInfo> {
    if s == "none" {
        return Ok(TimeoutWire::None);
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| invalid_argument(format!("invalid ttl `{s}`; use 30m, 2h, or none")))?;
    let ms = match unit {
        "s" => n * 1000,
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        _ => {
            return Err(invalid_argument(format!(
                "invalid ttl unit in `{s}`; use s, m, h, d, or none"
            )));
        }
    };
    Ok(TimeoutWire::After { ms })
}

async fn connect(ctx: &Ctx) -> Result<Client, ExitCode> {
    ctx.client(&ConnectOptions::cli())
        .await
        .map_err(|(c, e)| ctx.fail(c, e))
}

/// Polls until the run is finished (the host commits the final record first).
async fn wait_finished(
    client: &mut Client,
    run_id: &RunId,
) -> Result<PublicReply<RunRecord>, ErrorInfo> {
    loop {
        let reply: PublicReply<RunRecord> = client
            .call(
                Method::RunGet,
                &RunGetParams {
                    run_id: run_id.clone(),
                },
            )
            .await
            .map_err(|e| e.to_error_info())?;
        match reply.data() {
            Some(r) if !r.lifecycle.is_active() => return Ok(reply),
            Some(_) => tokio::time::sleep(POLL).await,
            None => return Ok(reply),
        }
    }
}

fn run_text(r: &RunRecord) -> String {
    let target = r
        .action_ref
        .as_ref()
        .map_or_else(|| format!("exec \"{}\"", r.label), ToString::to_string);
    let mut s = format!("{}  {}  {}", r.run_id, target, lifecycle_text(&r.lifecycle));
    if let Some(e) = &r.exit {
        match (e.code, &e.signal) {
            (Some(c), _) => s.push_str(&format!("  exit {c}")),
            (None, Some(sig)) => s.push_str(&format!("  signal {sig}")),
            _ => {}
        }
    }
    if let Some(n) = &r.note {
        s.push_str(&format!("\n  note: {n}"));
    }
    s
}

/// Converts a finished run into the public reply: success, or an error with the fixed class.
fn final_reply(reply: PublicReply<RunRecord>) -> PublicReply<RunRecord> {
    let ctx = reply.context();
    let Some(rec) = reply.data().cloned() else {
        return reply;
    };
    let outcome = match rec.lifecycle {
        Lifecycle::Finished { outcome } => outcome,
        _ => return reply,
    };
    let (code, what) = match outcome {
        Outcome::Succeeded => return reply,
        Outcome::Failed => (ErrorCode::EXECUTION_FAILED, "failed"),
        Outcome::TimedOut => (ErrorCode::TIMEOUT, "timed out"),
        Outcome::Cancelled => (ErrorCode::CANCELLED, "was cancelled"),
        Outcome::Interrupted => (ErrorCode::OUTCOME_UNKNOWN, "was interrupted"),
    };
    let target = rec
        .action_ref
        .as_ref()
        .map_or_else(|| rec.label.clone(), ToString::to_string);
    let exit = rec.exit.as_ref().map(|e| match (e.code, &e.signal) {
        (Some(c), _) => format!(" with exit {c}"),
        (None, Some(s)) => format!(" by {s}"),
        _ => String::new(),
    });
    let mut details = Map::new();
    details.insert("run_id".into(), Value::String(rec.run_id.to_string()));
    details.insert(
        "outcome".into(),
        serde_json::to_value(outcome).unwrap_or(Value::Null),
    );
    details.insert(
        "exit".into(),
        serde_json::to_value(&rec.exit).unwrap_or(Value::Null),
    );
    if let Some(n) = &rec.note {
        details.insert("note".into(), Value::String(n.clone()));
    }
    let info = ErrorInfo::new(code, format!("{target} {what}{}", exit.unwrap_or_default()))
        .with_details(details)
        .with_next_action(
            &["lyra", "logs", rec.run_id.as_str()],
            "Read the run's output.",
        );
    PublicReply::failure(ctx, info)
}

async fn run_and_wait(
    ctx: &Ctx,
    client: &mut Client,
    method: Method,
    params: Value,
    wait: bool,
) -> ExitCode {
    let accepted: PublicReply<InvokeAccepted> = match client.call(method, &params).await {
        Ok(r) => r,
        Err(e) => return ctx.fail(client.context(), e.to_error_info()),
    };
    if !wait || !accepted.is_ok() {
        return ctx.emit(&accepted, |a| {
            format!(
                "{}  {}{}",
                a.run_id,
                lifecycle_text(&a.state),
                if a.reused { "  (reused)" } else { "" }
            )
        });
    }
    let Some(run_id) = accepted.data().map(|a| a.run_id.clone()) else {
        return ctx.emit(&accepted, |_| String::new());
    };
    if ctx.mode == Mode::Text {
        eprintln!("{run_id} started; waiting (Ctrl-C stops it)...");
    }
    match wait_finished(client, &run_id).await {
        Ok(reply) => ctx.emit(&final_reply(reply), run_text),
        Err(e) => ctx.fail(client.context(), e),
    }
}

pub fn run(
    ctx: &Ctx,
    action: &str,
    input: Option<&str>,
    no_wait: bool,
    request_key: Option<String>,
) -> ExitCode {
    block_on(async {
        let prepared = (|| {
            Ok::<_, ErrorInfo>((
                parse_action(action)?,
                read_input(input)?,
                parse_key(request_key)?,
                env()?,
            ))
        })();
        let (action_ref, input, request_key, client_env) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = ActionInvokeParams {
            action_ref,
            input,
            client_env,
            request_key,
            foreground: !no_wait,
        };
        let params = serde_json::to_value(p).unwrap_or(Value::Null);
        run_and_wait(ctx, &mut client, Method::ActionInvoke, params, !no_wait).await
    })
}

pub fn start(
    ctx: &Ctx,
    action: &str,
    input: Option<&str>,
    request_key: Option<String>,
) -> ExitCode {
    block_on(async {
        let prepared = (|| {
            Ok::<_, ErrorInfo>((
                parse_action(action)?,
                read_input(input)?,
                parse_key(request_key)?,
                env()?,
            ))
        })();
        let (action_ref, input, request_key, client_env) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = ActionInvokeParams {
            action_ref,
            input,
            client_env,
            request_key,
            foreground: false,
        };
        run_and_wait(
            ctx,
            &mut client,
            Method::ActionInvoke,
            serde_json::to_value(p).unwrap_or(Value::Null),
            false,
        )
        .await
    })
}

pub fn exec(ctx: &Ctx, label: String, argv: Vec<String>, request_key: Option<String>) -> ExitCode {
    block_on(async {
        let prepared = (|| Ok::<_, ErrorInfo>((parse_key(request_key)?, env()?)))();
        let (request_key, client_env) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = ActionExecParams {
            label,
            argv,
            client_env,
            request_key,
            foreground: true,
        };
        run_and_wait(
            ctx,
            &mut client,
            Method::ActionExec,
            serde_json::to_value(p).unwrap_or(Value::Null),
            true,
        )
        .await
    })
}

async fn stop_target(
    ctx: &Ctx,
    client: &mut Client,
    target: RunTarget,
    wait: bool,
) -> Result<PublicReply<StopAccepted>, ExitCode> {
    let reply: PublicReply<StopAccepted> = client
        .call(Method::RunStop, &RunStopParams { target })
        .await
        .map_err(|e| ctx.fail(client.context(), e.to_error_info()))?;
    if wait && let Some(run_id) = reply.data().map(|d| d.run_id.clone()) {
        let fin = wait_finished(client, &run_id)
            .await
            .map_err(|e| ctx.fail(client.context(), e))?;
        let ctx2 = fin.context();
        if let Some(rec) = fin.data() {
            return Ok(PublicReply::success(
                ctx2,
                StopAccepted {
                    run_id,
                    state: rec.lifecycle,
                },
                reply.meta().clone(),
            ));
        }
    }
    Ok(reply)
}

pub fn stop(ctx: &Ctx, target: &str, wait: bool) -> ExitCode {
    block_on(async {
        let target = match parse_target(target) {
            Ok(t) => t,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        match stop_target(ctx, &mut client, target, wait).await {
            Ok(reply) => ctx.emit(&reply, |s| {
                format!("{}  {}", s.run_id, lifecycle_text(&s.state))
            }),
            Err(code) => code,
        }
    })
}

pub fn restart(ctx: &Ctx, action: &str, input: Option<&str>) -> ExitCode {
    block_on(async {
        let prepared =
            (|| Ok::<_, ErrorInfo>((parse_action(action)?, read_input(input)?, env()?)))();
        let (action_ref, input, client_env) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        // Stop the current instance (if any) and wait, then start with the current definition.
        let stopped = client
            .call::<_, StopAccepted>(
                Method::RunStop,
                &RunStopParams {
                    target: RunTarget::Action {
                        action_ref: action_ref.clone(),
                    },
                },
            )
            .await;
        if let Ok(reply) = stopped
            && let Some(run_id) = reply.data().map(|d| d.run_id.clone())
            && let Err(e) = wait_finished(&mut client, &run_id).await
        {
            return ctx.fail(client.context(), e);
        }
        let p = ActionInvokeParams {
            action_ref,
            input,
            client_env,
            request_key: None,
            foreground: false,
        };
        run_and_wait(
            ctx,
            &mut client,
            Method::ActionInvoke,
            serde_json::to_value(p).unwrap_or(Value::Null),
            false,
        )
        .await
    })
}

fn session_text(d: &SessionData) -> String {
    match &d.session {
        None => "no session".into(),
        Some(s) => format!(
            "session {} {} {}, {} controller(s){}",
            s.id,
            super::inspect::lc(&s.mode),
            super::inspect::lc(&s.state),
            s.controller_count,
            s.expires_at
                .map(|t| format!(", expires {t}"))
                .unwrap_or_else(|| ", no expiry".into())
        ),
    }
}

pub fn up(ctx: &Ctx, background: bool, ttl: &str) -> ExitCode {
    if !background {
        return ctx.fail(
            ReplyContext::default(),
            invalid_argument("`lyra up` needs --background; open `lyra` for a foreground session")
                .with_next_action(
                    &["lyra", "up", "--background"],
                    "Keep work running without an open TUI.",
                ),
        );
    }
    block_on(async {
        let prepared = (|| Ok::<_, ErrorInfo>((parse_ttl(ttl)?, env()?)))();
        let (ttl, client_env) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        match client
            .call::<_, SessionData>(
                Method::SessionOpen,
                &SessionOpenParams {
                    mode: OpenMode::Background,
                    client_env,
                    ttl,
                },
            )
            .await
        {
            Ok(r) => ctx.emit(&r, session_text),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn keep(ctx: &Ctx, ttl: &str) -> ExitCode {
    block_on(async {
        let ttl = match parse_ttl(ttl) {
            Ok(t) => t,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        match client
            .call::<_, SessionData>(Method::SessionKeep, &SessionKeepParams { ttl })
            .await
        {
            Ok(r) => ctx.emit(&r, session_text),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn down(ctx: &Ctx, wait: bool) -> ExitCode {
    block_on(async {
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let reply = match client
            .call::<_, SessionData>(Method::SessionStop, &Empty {})
            .await
        {
            Ok(r) => r,
            Err(e) => return ctx.fail(client.context(), e.to_error_info()),
        };
        if wait {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            loop {
                match client.status().await {
                    Ok(s) if s.data().is_some_and(|d| d.session.is_none()) => {
                        return ctx
                            .emit(&s.map(|d| SessionData { session: d.session }), session_text);
                    }
                    Ok(_) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(POLL).await
                    }
                    Ok(_) => {
                        return ctx.fail(
                            client.context(),
                            ErrorInfo::new(
                                ErrorCode::TIMEOUT,
                                "the session is still stopping after 30 s",
                            ),
                        );
                    }
                    Err(e) => return ctx.fail(client.context(), e.to_error_info()),
                }
            }
        }
        ctx.emit(&reply, session_text)
    })
}

pub fn runs(
    ctx: &Ctx,
    run: Option<String>,
    action: Option<String>,
    outcome: Option<String>,
    limit: Option<u32>,
    after: Option<String>,
) -> ExitCode {
    block_on(async {
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        if let Some(run) = run {
            let run_id = match RunId::parse(run) {
                Ok(r) => r,
                Err(e) => return ctx.fail(client.context(), invalid_argument(e.to_string())),
            };
            return match client
                .call::<_, RunRecord>(Method::RunGet, &RunGetParams { run_id })
                .await
            {
                Ok(r) => ctx.emit(&r, |rec| {
                    serde_json::to_string_pretty(rec).unwrap_or_else(|_| run_text(rec))
                }),
                Err(e) => ctx.fail(client.context(), e.to_error_info()),
            };
        }
        let parsed = (|| {
            let action_ref = action.as_deref().map(parse_action).transpose()?;
            let outcome = outcome
                .map(|o| {
                    serde_json::from_value::<Outcome>(Value::String(o.clone()))
                        .map_err(|_| invalid_argument(format!("unknown outcome `{o}`")))
                })
                .transpose()?;
            Ok::<_, ErrorInfo>((action_ref, outcome))
        })();
        let (action_ref, outcome) = match parsed {
            Ok(v) => v,
            Err(e) => return ctx.fail(client.context(), e),
        };
        let p = RunListParams {
            action_ref,
            outcome,
            cursor: after,
            limit,
        };
        match client.call::<_, RunList>(Method::RunListM, &p).await {
            Ok(r) => ctx.emit(&r, |l| {
                if l.runs.is_empty() {
                    "no runs".into()
                } else {
                    l.runs.iter().map(run_text).collect::<Vec<_>>().join("\n")
                }
            }),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

fn log_line(r: &lyra_protocol::run::LogRecord) -> String {
    let tag = match r.stream {
        lyra_protocol::run::LogStream::Stderr => "err ",
        lyra_protocol::run::LogStream::Host => "lyra",
        lyra_protocol::run::LogStream::Plugin => "plug",
        lyra_protocol::run::LogStream::Pty => "pty ",
        lyra_protocol::run::LogStream::Stdout => "out ",
    };
    format!("{:>6} {tag} {}", r.log_seq, r.text)
}

pub fn logs(
    ctx: &Ctx,
    target: &str,
    after: Option<String>,
    limit: Option<u32>,
    max_bytes: Option<u32>,
    follow: bool,
) -> ExitCode {
    block_on(async {
        let target = match parse_target(target) {
            Ok(t) => t,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = LogReadParams {
            target: target.clone(),
            cursor: after,
            limit,
            max_bytes,
        };
        let page: PublicReply<LogPage> = match client.call(Method::LogRead, &p).await {
            Ok(r) => r,
            Err(e) => return ctx.fail(client.context(), e.to_error_info()),
        };
        if !follow || !page.is_ok() {
            return ctx.emit(&page, |pg| {
                pg.items.iter().map(log_line).collect::<Vec<_>>().join("\n")
            });
        }
        let Some(tail) = page.data().cloned() else {
            return ctx.emit(&page, |_| String::new());
        };
        follow_logs(ctx, tail).await
    })
}

/// `--follow`: JSONL stream frames (ready first), or text lines after the tail.
async fn follow_logs(ctx: &Ctx, tail: LogPage) -> ExitCode {
    let opts = ConnectOptions::cli().kind(ClientKind::Cli, ConnectionKind::Stream);
    let mut stream = match ctx.client(&opts).await {
        Ok(c) => c,
        Err((c, e)) => return ctx.fail(c, e),
    };
    let run_id = tail.run_id.clone();
    let sub = StreamSubscribeParams {
        kinds: vec![StreamKind::Log, StreamKind::State],
        refs: vec![run_id.to_string()],
        cursor: None,
    };
    if let Err(e) = stream
        .call::<_, Subscribed>(Method::StreamSubscribe, &sub)
        .await
    {
        return ctx.fail(stream.context(), e.to_error_info());
    }
    let last_seen = tail.items.last().map(|r| r.log_seq);
    let mut out = std::io::stdout().lock();
    if ctx.mode == Mode::Text {
        for r in &tail.items {
            let _ = writeln!(out, "{}", log_line(r));
        }
    }
    loop {
        let frame = match stream.next_event().await {
            Ok(f) => f,
            Err(e) => return output::fail(ctx.mode, stream.context(), e.to_error_info()),
        };
        let mut done = false;
        match &frame.event {
            StreamEvent::Log { records, .. } => {
                if ctx.mode == Mode::Text {
                    for r in records
                        .iter()
                        .filter(|r| last_seen.is_none_or(|s| r.log_seq > s))
                    {
                        let _ = writeln!(out, "{}", log_line(r));
                    }
                }
            }
            StreamEvent::State { runs, .. } | StreamEvent::Snapshot(StatusData { runs, .. }) => {
                done = !runs.iter().any(|r| r.run_id == run_id);
            }
            StreamEvent::End { .. } => done = true,
            _ => {}
        }
        let relevant = !matches!(
            frame.event,
            StreamEvent::State { .. } | StreamEvent::Snapshot(_)
        );
        if ctx.mode == Mode::Json
            && (relevant || done)
            && let Ok(line) = serde_json::to_string(&frame)
        {
            let _ = writeln!(out, "{line}");
        }
        let _ = out.flush();
        if done {
            return ExitCode::SUCCESS;
        }
    }
}
