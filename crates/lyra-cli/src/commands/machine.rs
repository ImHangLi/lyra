//! Machine-wide commands: `lyra ps` lists every Lyra host of this user with its session, runs,
//! ports, and memory; `lyra down --all` stops the sessions of all of them. Hosts are found from
//! owner records in the runtime directory and are never started by these commands.

use std::collections::{BTreeSet, HashMap};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use lyra_client::{Client, ConnectOptions, connect};
use lyra_protocol::ids::AbsolutePath;
use lyra_protocol::ipc::*;
use lyra_protocol::paths::{WorkspacePaths, current_uid, runtime_dir};
use lyra_protocol::reply::{PublicReply, ReplyContext, ReplyMeta};
use lyra_protocol::{ErrorCode, Timestamp, schemas};
use serde::Serialize;
use serde_json::Value;

use super::ctx::{Ctx, block_on};
use super::inspect::lifecycle_text;
use super::procs::{self, Flag, FlagInput, FlagRun, Snapshot, human_duration};
use super::runtime::{OtherBuildStop, stop_other_build_at};

/// Budget for one host's answers in `lyra ps`; a slower host is listed as unreachable.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const STOP_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
const MAX_OWNER_BYTES: u64 = 64 * 1024;

/// A live host found from its owner record.
#[derive(Debug, Clone)]
struct Host {
    paths: WorkspacePaths,
    pid: u32,
    version: String,
    started_at: Option<String>,
    other_build: bool,
}

/// Reads the owner records of this user's live hosts. Files and directories owned by another
/// user are ignored, and a record is trusted only when its root maps to its file name.
fn discover() -> Vec<Host> {
    let dir = runtime_dir();
    let uid = current_uid();
    let owned = |p: &Path, dir: bool| {
        std::fs::symlink_metadata(p)
            .is_ok_and(|m| m.uid() == uid && if dir { m.is_dir() } else { m.is_file() })
    };
    if !owned(&dir, true) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let ours = serde_json::to_value(schemas::protocol_hash()).unwrap_or(Value::Null);
    let mut hosts: Vec<Host> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let stem = path
                .file_name()?
                .to_str()?
                .strip_suffix(".owner")?
                .to_owned();
            let meta = std::fs::symlink_metadata(&path).ok()?;
            if !meta.is_file() || meta.uid() != uid || meta.len() > MAX_OWNER_BYTES {
                return None;
            }
            let owner: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            let root = AbsolutePath::parse(owner.get("root")?.as_str()?.to_owned()).ok()?;
            let paths = WorkspacePaths::new(root);
            if paths.id.as_str() != stem {
                return None;
            }
            let pid = u32::try_from(owner.get("pid")?.as_u64()?).ok()?;
            let raw = i32::try_from(pid).ok()?;
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(raw)?).ok()?;
            let text = |k: &str| owner.get(k).and_then(Value::as_str).map(str::to_owned);
            Some(Host {
                pid,
                version: text("version").unwrap_or_else(|| "unknown".into()),
                started_at: text("started_at"),
                other_build: owner.get("protocol_hash") != Some(&ours),
                paths,
            })
        })
        .collect();
    hosts.sort_by(|a, b| a.paths.root.as_str().cmp(b.paths.root.as_str()));
    hosts
}

fn workspace_name(root: &Path) -> String {
    std::fs::read(root.join(".lyra/workspace.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("name")?.as_str().map(str::to_owned))
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| {
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string())
        })
}

fn short_root(root: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if home.len() > 1 && root.starts_with(&home) => {
            let rest = &root[home.len()..];
            if rest.is_empty() || rest.starts_with('/') {
                return format!("~{rest}");
            }
            root.to_owned()
        }
        _ => root.to_owned(),
    }
}

fn no_spawn() -> ConnectOptions {
    ConnectOptions {
        spawn: false,
        ..ConnectOptions::cli()
    }
}

/// A host's status and the mode of each active action, or why it could not be read.
type Probe = Result<(StatusData, HashMap<String, String>), String>;

/// Status plus the mode of each active action, from one connection.
async fn probe(paths: WorkspacePaths) -> Probe {
    let mut client = connect(&paths, &no_spawn())
        .await
        .map_err(|e| e.to_string())?;
    let status = client
        .status()
        .await
        .map_err(|e| e.to_string())?
        .data()
        .cloned()
        .ok_or("no status")?;
    let mut modes = HashMap::new();
    for r in &status.runs {
        let Some(action) = &r.action_ref else {
            continue;
        };
        let key = action.to_string();
        if modes.contains_key(&key) {
            continue;
        }
        let params = ItemDescribeParams {
            item_ref: action.to_item_ref(),
            include_schema: false,
            max_bytes: None,
        };
        if let Ok(d) = client
            .call::<_, ItemDescription>(Method::ItemDescribe, &params)
            .await
            && let Some(a) = d.data().and_then(|d| d.action.as_ref())
        {
            let mut mode = super::inspect::lc(&a.mode);
            if a.terminal == lyra_protocol::manifest::TerminalMode::Pty {
                mode.push_str("/pty");
            }
            modes.insert(key, mode);
        }
    }
    Ok((status, modes))
}

#[derive(Debug, Serialize)]
struct PsHost {
    pid: u32,
    version: String,
    /// `current`, or `older` for a host from another Lyra build (the protocol differs).
    build: &'static str,
    started_at: Option<String>,
    /// Resident memory of the host process itself.
    memory_bytes: Option<u64>,
    reachable: bool,
}

#[derive(Debug, Serialize)]
struct PsRun {
    run_id: String,
    action_ref: Option<String>,
    mode: Option<String>,
    state: String,
    started_at: Timestamp,
    age_ms: i64,
    /// Leader of the run's process group (from the host's process-group ledger).
    pgid: Option<u32>,
    group_alive: bool,
    processes: u32,
    /// Resident memory of the whole process group.
    memory_bytes: u64,
    ports: Vec<u16>,
}

#[derive(Debug, Serialize)]
struct PsWorkspace {
    id: String,
    name: String,
    root: String,
    branch: Option<String>,
    host: PsHost,
    session: Option<SessionInfo>,
    runs: Vec<PsRun>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct PsTotals {
    workspaces: usize,
    running_runs: usize,
    memory_bytes: u64,
    ports: Vec<u16>,
}

#[derive(Debug, Serialize)]
struct PsData {
    workspaces: Vec<PsWorkspace>,
    totals: PsTotals,
    flags: Vec<Flag>,
}

fn ledger_pgid(paths: &WorkspacePaths, run_id: &str) -> Option<u32> {
    let text = std::fs::read_to_string(paths.state_dir.join("process-groups").join(run_id)).ok()?;
    text.lines().next()?.trim().parse().ok()
}

/// Runs known only from the ledger: the group leader and when the entry was written.
fn ledger_runs(paths: &WorkspacePaths, snap: &Snapshot, now: Timestamp) -> Vec<PsRun> {
    let Ok(entries) = std::fs::read_dir(paths.state_dir.join("process-groups")) else {
        return Vec::new();
    };
    let mut runs: Vec<PsRun> = entries
        .flatten()
        .filter_map(|e| {
            let run_id = e.file_name().to_str()?.to_owned();
            let pgid = ledger_pgid(paths, &run_id)?;
            let written = e.metadata().ok()?.modified().ok()?;
            let unix_ms = written
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis();
            let started_at = Timestamp::from_unix_ms(i64::try_from(unix_ms).ok()?);
            let group = snap.group(pgid);
            Some(PsRun {
                run_id,
                action_ref: None,
                mode: None,
                state: "unknown".into(),
                started_at,
                age_ms: now.unix_ms() - started_at.unix_ms(),
                pgid: Some(pgid),
                group_alive: group.is_some(),
                processes: group.map_or(0, |g| g.processes),
                memory_bytes: group.map_or(0, |g| g.rss_kib * 1024),
                ports: group
                    .map(|g| g.ports.iter().copied().collect())
                    .unwrap_or_default(),
            })
        })
        .collect();
    runs.sort_by_key(|r| r.started_at);
    runs
}

fn build_workspace(
    host: &Host,
    probed: Option<Probe>,
    snap: &Snapshot,
    now: Timestamp,
) -> PsWorkspace {
    let root = host.paths.root.as_path();
    let mut ws = PsWorkspace {
        id: host.paths.id.to_string(),
        name: workspace_name(root),
        root: host.paths.root.to_string(),
        branch: lyra_tui::git::head_label(root),
        host: PsHost {
            pid: host.pid,
            version: host.version.clone(),
            build: if host.other_build { "older" } else { "current" },
            started_at: host.started_at.clone(),
            memory_bytes: snap.rss_by_pid.get(&host.pid).map(|k| k * 1024),
            reachable: false,
        },
        session: None,
        runs: Vec::new(),
        error: None,
    };
    match probed {
        // A host from another build cannot be asked; its process-group ledger still shows
        // which work it owns.
        None => ws.runs = ledger_runs(&host.paths, snap, now),
        Some(Err(e)) => ws.error = Some(e),
        Some(Ok((status, modes))) => {
            ws.host.reachable = true;
            ws.session = status.session;
            ws.runs = status
                .runs
                .iter()
                .filter(|r| r.lifecycle.is_active())
                .map(|r| {
                    let pgid = ledger_pgid(&host.paths, r.run_id.as_str());
                    let group = pgid.and_then(|g| snap.group(g));
                    let action = r.action_ref.as_ref().map(ToString::to_string);
                    PsRun {
                        run_id: r.run_id.to_string(),
                        mode: match &action {
                            Some(a) => modes.get(a).cloned(),
                            None => Some("exec".into()),
                        },
                        action_ref: action,
                        state: lifecycle_text(&r.lifecycle),
                        started_at: r.started_at,
                        age_ms: now.unix_ms() - r.started_at.unix_ms(),
                        pgid,
                        group_alive: group.is_some(),
                        processes: group.map_or(0, |g| g.processes),
                        memory_bytes: group.map_or(0, |g| g.rss_kib * 1024),
                        ports: group
                            .map(|g| g.ports.iter().copied().collect())
                            .unwrap_or_default(),
                    }
                })
                .collect();
        }
    }
    ws
}

fn flag_inputs<'a>(workspaces: &'a [PsWorkspace], now: Timestamp) -> Vec<FlagInput<'a>> {
    workspaces
        .iter()
        .map(|w| FlagInput {
            name: &w.name,
            other_build: (w.host.build == "older").then_some(w.host.version.as_str()),
            unreachable: w.host.build == "current" && !w.host.reachable,
            background_expires_in_ms: w
                .session
                .as_ref()
                .filter(|s| s.mode == SessionMode::Background)
                .and_then(|s| s.expires_at)
                .map(|t| t.unix_ms() - now.unix_ms()),
            runs: w
                .runs
                .iter()
                .map(|r| FlagRun {
                    label: r.action_ref.as_deref().unwrap_or(&r.run_id),
                    run_id: &r.run_id,
                    running: r.state == "running",
                    age_ms: r.age_ms,
                    pgid: r.pgid,
                    group_alive: r.group_alive,
                    ports: r.ports.clone(),
                })
                .collect(),
        })
        .collect()
}

pub fn ps(ctx: &Ctx) -> ExitCode {
    block_on(async {
        let hosts = discover();
        // Nothing to measure without a host: skip the process listings.
        let snap = tokio::task::spawn_blocking({
            let any = !hosts.is_empty();
            move || {
                if any {
                    Snapshot::collect()
                } else {
                    Snapshot::default()
                }
            }
        });
        let probes: Vec<_> = hosts
            .iter()
            .map(|h| {
                (!h.other_build).then(|| {
                    let paths = h.paths.clone();
                    tokio::spawn(async move {
                        tokio::time::timeout(PROBE_TIMEOUT, probe(paths))
                            .await
                            .unwrap_or_else(|_| {
                                Err(format!("no answer within {} ms", PROBE_TIMEOUT.as_millis()))
                            })
                    })
                })
            })
            .collect();
        let snap = snap.await.unwrap_or_default();
        let now = Timestamp::now();
        let mut workspaces = Vec::with_capacity(hosts.len());
        for (host, probe) in hosts.iter().zip(probes) {
            let probed = match probe {
                Some(h) => Some(h.await.unwrap_or_else(|e| Err(e.to_string()))),
                None => None,
            };
            workspaces.push(build_workspace(host, probed, &snap, now));
        }
        let flags = procs::flags(&flag_inputs(&workspaces, now));
        let ports: BTreeSet<u16> = workspaces
            .iter()
            .flat_map(|w| w.runs.iter().flat_map(|r| r.ports.iter().copied()))
            .collect();
        let totals = PsTotals {
            workspaces: workspaces.len(),
            running_runs: workspaces.iter().map(|w| w.runs.len()).sum(),
            memory_bytes: workspaces
                .iter()
                .map(|w| {
                    w.host.memory_bytes.unwrap_or(0)
                        + w.runs.iter().map(|r| r.memory_bytes).sum::<u64>()
                })
                .sum(),
            ports: ports.into_iter().collect(),
        };
        let data = PsData {
            workspaces,
            totals,
            flags,
        };
        let reply = PublicReply::success(ReplyContext::default(), data, ReplyMeta::default());
        ctx.emit(&reply, ps_text)
    })
}

pub fn memory(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.2} GB", b / KB / KB / KB)
    }
}

fn session_cell(w: &PsWorkspace, now: i64) -> String {
    if w.host.build == "older" {
        return "unknown".into();
    }
    if !w.host.reachable {
        return "unreachable".into();
    }
    let Some(s) = &w.session else {
        return "none".into();
    };
    let mut out = match s.mode {
        SessionMode::Foreground => format!("fg, {} ctl", s.controller_count),
        SessionMode::Background => match s.expires_at {
            Some(t) => format!("bg, {} left", human_duration(t.unix_ms() - now)),
            None => "bg, no expiry".into(),
        },
    };
    if s.state == SessionState::Stopping {
        out.push_str(", stopping");
    }
    out
}

/// Cuts `s` to `max` characters, keeping the end (the most specific part of a path).
fn cut_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 2 {
        return s.to_owned();
    }
    format!("…{}", s.chars().skip(n - max + 1).collect::<String>())
}

fn cut_right(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    format!("{}…", s.chars().take(max - 1).collect::<String>())
}

fn ports_text(ports: &[u16]) -> String {
    ports
        .iter()
        .map(|p| format!(":{p}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn ps_text(d: &PsData) -> String {
    if d.workspaces.is_empty() {
        return "no Lyra host is running for this user".into();
    }
    let now = Timestamp::now().unix_ms();
    let rows: Vec<[String; 5]> = d
        .workspaces
        .iter()
        .map(|w| {
            let version = if w.host.build == "older" {
                format!("{} older build", w.host.version)
            } else {
                w.host.version.clone()
            };
            [
                cut_right(&w.name, 22),
                cut_right(w.branch.as_deref().unwrap_or("-"), 20),
                session_cell(w, now),
                version,
                w.host.memory_bytes.map_or("-".into(), memory),
            ]
        })
        .collect();
    let width = |i: usize, head: &str| {
        rows.iter()
            .map(|r| r[i].chars().count())
            .chain([head.len()])
            .max()
            .unwrap_or(0)
    };
    let heads = ["WORKSPACE", "BRANCH", "SESSION", "LYRA", "HOST MEM"];
    let w: Vec<usize> = heads.iter().enumerate().map(|(i, h)| width(i, h)).collect();
    let used: usize = w.iter().sum::<usize>() + 2 * heads.len();
    let root_max = 100usize.saturating_sub(used).max(16);
    let line = |c: [&str; 5], root: &str| {
        format!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:>w4$}  {root}",
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            w0 = w[0],
            w1 = w[1],
            w2 = w[2],
            w3 = w[3],
            w4 = w[4],
        )
    };
    let mut out = vec![line(heads, "ROOT")];
    // Run rows share one set of column widths across workspaces.
    let run_cells = |r: &PsRun| -> [String; 6] {
        [
            match (&r.action_ref, r.mode.as_deref()) {
                (Some(a), _) => a.clone(),
                (None, Some(m)) => m.to_owned(),
                (None, None) => cut_right(&r.run_id, 12),
            },
            r.mode.clone().unwrap_or_else(|| "-".into()),
            r.state.clone(),
            human_duration(r.age_ms),
            r.pgid.map_or("pid -".into(), |p| format!("pid {p}")),
            if r.group_alive {
                memory(r.memory_bytes)
            } else {
                "gone".into()
            },
        ]
    };
    let all_runs: Vec<[String; 6]> = d
        .workspaces
        .iter()
        .flat_map(|w| w.runs.iter().map(run_cells))
        .collect();
    let rw: Vec<usize> = (0..6)
        .map(|i| {
            all_runs
                .iter()
                .map(|c| c[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for (ws, row) in d.workspaces.iter().zip(&rows) {
        let cells = [
            row[0].as_str(),
            row[1].as_str(),
            row[2].as_str(),
            row[3].as_str(),
            row[4].as_str(),
        ];
        out.push(line(cells, &cut_left(&short_root(&ws.root), root_max)));
        for r in &ws.runs {
            let c = run_cells(r);
            out.push(
                format!(
                    "  {:<a$}  {:<b$}  {:<s$}  {:>g$}  {:<p$}  {:>m$}  {}",
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    c[5],
                    ports_text(&r.ports),
                    a = rw[0],
                    b = rw[1],
                    s = rw[2],
                    g = rw[3],
                    p = rw[4],
                    m = rw[5],
                )
                .trim_end()
                .to_owned(),
            );
        }
        if let Some(e) = &ws.error {
            out.push(format!("  error: {e}"));
        }
    }
    for f in &d.flags {
        out.push(format!("! {}", f.message));
    }
    let t = &d.totals;
    let ports = if t.ports.is_empty() {
        "no listening ports".to_owned()
    } else {
        format!("ports {}", ports_text(&t.ports))
    };
    out.push(format!(
        "{} workspace(s) · {} active run(s) · {} · {ports}",
        t.workspaces,
        t.running_runs,
        memory(t.memory_bytes)
    ));
    out.join("\n")
}

#[derive(Debug, Serialize)]
struct DownWorkspace {
    id: String,
    name: String,
    root: String,
    build: &'static str,
    stopped_session: Option<String>,
    stopped_runs: u32,
    /// True when the whole host was stopped (hosts from another build).
    host_stopped: bool,
    /// True when the session (or host) has finished stopping; only waited for with `--wait`.
    done: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DownTotals {
    workspaces: usize,
    sessions: usize,
    runs: u32,
    hosts: usize,
    failed: usize,
}

#[derive(Debug, Serialize)]
struct DownData {
    workspaces: Vec<DownWorkspace>,
    totals: DownTotals,
}

async fn wait_idle(client: &mut Client) -> Result<bool, String> {
    let deadline = tokio::time::Instant::now() + STOP_WAIT;
    loop {
        let s = client.status().await.map_err(|e| e.to_string())?;
        if s.data()
            .is_some_and(|d| d.session.is_none() && !d.runs.iter().any(|r| r.lifecycle.is_active()))
        {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Stops a host from another build through the verified SIGTERM path of `lyra down`.
async fn stop_older(paths: &WorkspacePaths, wait: bool, mut out: DownWorkspace) -> DownWorkspace {
    let limit = if wait { STOP_WAIT } else { Duration::ZERO };
    match stop_other_build_at(paths, limit).await {
        OtherBuildStop::NotVerified => {
            out.error = Some("the owner record or process did not match; not stopped".into())
        }
        OtherBuildStop::Signalled { exited, .. } => {
            out.build = "older";
            out.host_stopped = true;
            out.done = exited;
            if wait && !exited {
                out.error = Some("the older host is still stopping after 30 s".into());
            }
        }
    }
    out
}

async fn stop_one(host: Host, wait: bool) -> DownWorkspace {
    let mut out = DownWorkspace {
        id: host.paths.id.to_string(),
        name: workspace_name(host.paths.root.as_path()),
        root: host.paths.root.to_string(),
        build: if host.other_build { "older" } else { "current" },
        stopped_session: None,
        stopped_runs: 0,
        host_stopped: false,
        done: false,
        error: None,
    };
    if host.other_build {
        return stop_older(&host.paths, wait, out).await;
    }
    let mut client = match connect(&host.paths, &no_spawn()).await {
        Ok(c) => c,
        Err(e) if e.to_error_info().code == ErrorCode::PROTOCOL_MISMATCH => {
            return stop_older(&host.paths, wait, out).await;
        }
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    };
    match client
        .call::<_, SessionStopData>(Method::SessionStop, &Empty {})
        .await
    {
        Ok(r) => match (r.data(), r.error()) {
            (Some(d), _) => {
                out.stopped_session = d.stopped_session.as_ref().map(ToString::to_string);
                out.stopped_runs = d.stopped_runs;
            }
            (None, Some(e)) => out.error = Some(e.message.clone()),
            (None, None) => {}
        },
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    }
    if wait && out.error.is_none() {
        match wait_idle(&mut client).await {
            Ok(true) => out.done = true,
            Ok(false) => out.error = Some("the session is still stopping after 30 s".into()),
            Err(e) => out.error = Some(e),
        }
    }
    out
}

fn down_text(d: &DownData) -> String {
    if d.workspaces.is_empty() {
        return "no Lyra host is running for this user".into();
    }
    let w = d
        .workspaces
        .iter()
        .map(|w| w.name.chars().count().min(24))
        .max()
        .unwrap_or(0);
    let mut out: Vec<String> = d
        .workspaces
        .iter()
        .map(|ws| {
            let what = match (&ws.error, ws.host_stopped, &ws.stopped_session) {
                (Some(e), _, _) => format!("error: {e}"),
                (None, true, _) => "stopped the host from another build and its work".into(),
                (None, false, Some(id)) => {
                    format!("stopped session {id} and {} run(s)", ws.stopped_runs)
                }
                (None, false, None) => "no session".into(),
            };
            format!("{:<w$}  {what}", cut_right(&ws.name, 24))
        })
        .collect();
    let t = &d.totals;
    out.push(format!(
        "{} workspace(s) · {} session(s) and {} run(s) stopped · {} older host(s) stopped{}",
        t.workspaces,
        t.sessions,
        t.runs,
        t.hosts,
        if t.failed > 0 {
            format!(" · {} failed", t.failed)
        } else {
            String::new()
        }
    ));
    out.join("\n")
}

/// `lyra down --all`: stops the session of every running host of this user. No data is deleted.
pub fn down_all(ctx: &Ctx, wait: bool) -> ExitCode {
    block_on(async {
        let handles: Vec<_> = discover()
            .into_iter()
            .map(|h| tokio::spawn(stop_one(h, wait)))
            .collect();
        let mut workspaces = Vec::with_capacity(handles.len());
        for h in handles {
            if let Ok(w) = h.await {
                workspaces.push(w);
            }
        }
        let totals = DownTotals {
            workspaces: workspaces.len(),
            sessions: workspaces
                .iter()
                .filter(|w| w.stopped_session.is_some())
                .count(),
            runs: workspaces.iter().map(|w| w.stopped_runs).sum(),
            hosts: workspaces.iter().filter(|w| w.host_stopped).count(),
            failed: workspaces.iter().filter(|w| w.error.is_some()).count(),
        };
        let reply = PublicReply::success(
            ReplyContext::default(),
            DownData { workspaces, totals },
            ReplyMeta::default(),
        );
        ctx.emit(&reply, down_text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_under_home_are_shortened() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(short_root(&format!("{home}/src/app")), "~/src/app");
        assert_eq!(short_root(&format!("{home}x/app")), format!("{home}x/app"));
        assert_eq!(short_root("/tmp/lp/a"), "/tmp/lp/a");
    }

    #[test]
    fn cells_are_cut_to_width() {
        assert_eq!(cut_left("/a/very/long/path", 8), "…ng/path");
        assert_eq!(cut_right("feature/long-branch", 8), "feature…");
        assert_eq!(memory(512 * 1024), "512 KB");
        assert_eq!(memory(25 * 1024 * 1024 + 300 * 1024), "25.3 MB");
    }
}
