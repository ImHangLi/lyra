//! The workspace actor: the only writer of workspace state (§3.1, §9.4).
//!
//! Requests arrive as messages; storage commits and runner facts come back as messages.
//! The actor never awaits a child process, a file scan, or a storage commit inline.

mod artifacts;
mod configure;
mod plugin;
mod runs;
mod schedule;
mod session;
mod streams;
mod views;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use lyra_protocol::config::{self, ConfigSet};
use lyra_protocol::error::{ErrorCode, ErrorInfo, Issues};
use lyra_protocol::ids::*;
use lyra_protocol::ipc::*;
use lyra_protocol::manifest::{ActionMode, Runner};
use lyra_protocol::paths::WorkspacePaths;
use lyra_protocol::reply::{PublicReply, ReplyContext, ReplyMeta, WorkspaceRef};
use lyra_protocol::run::RunRecord;
use lyra_protocol::schemas;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::diag;
use crate::logs::SharedLog;
use crate::runner::RunnerEvent;
use crate::server::{error_line, result_line};
use crate::storage::{Claim, Storage, StorageError};

use runs::ActiveRun;
pub use runs::Reservation;
use session::Session;
use streams::Sub;

/// Observe-only hosts exit this long after the last client leaves with no session.
const IDLE_EXIT: Duration = Duration::from_secs(5);
const DEFAULT_CATALOG_LIMIT: usize = 30;
pub(crate) const MAX_LIMIT: usize = 1000;
const RECENT_RUNS: usize = 64;

/// Per-connection channels: control responses are never dropped; events are bounded.
#[derive(Clone)]
pub struct Outbound {
    responses: mpsc::UnboundedSender<Vec<u8>>,
    events: mpsc::Sender<Vec<u8>>,
}

impl Outbound {
    pub fn new(responses: mpsc::UnboundedSender<Vec<u8>>, events: mpsc::Sender<Vec<u8>>) -> Self {
        Self { responses, events }
    }
    pub fn respond(&self, line: Vec<u8>) {
        let _ = self.responses.send(line);
    }
    /// Returns false when the subscriber is too slow and must be reset.
    pub fn event(&self, line: Vec<u8>) -> bool {
        self.events.try_send(line).is_ok()
    }
}

/// Answers one request, now or after an asynchronous step.
pub struct Responder {
    out: Outbound,
    id: String,
}

impl Responder {
    pub fn send(self, result: Handled) {
        self.out.respond(match result {
            Ok(v) => result_line(self.id, v),
            Err(e) => error_line(Some(self.id), e),
        });
    }
}

pub type Handled = Result<Value, RpcError>;

pub enum Msg {
    Hello {
        params: HelloParams,
        out: Outbound,
        reply: oneshot::Sender<Result<(ClientId, HelloReply), RpcError>>,
    },
    Request {
        client: ClientId,
        id: String,
        method: Method,
        params: Map<String, Value>,
    },
    Closed {
        client: ClientId,
    },
    Reserved {
        run_id: RunId,
        result: Result<Reservation, ErrorInfo>,
    },
    FinalSaved {
        run_id: RunId,
        error: Option<String>,
    },
    ViewBlock {
        result: Result<(u64, u64), StorageError>,
    },
    ViewClaimed {
        result: Result<Claim, StorageError>,
    },
    ViewSaved {
        view_ref: ViewRef,
        revision: ViewRevision,
        error: Option<String>,
    },
    ScheduleSaved {
        params: ScheduleSetParams,
        saved: Result<(), ErrorInfo>,
        responder: Responder,
    },
    ConfigDone {
        result: Result<configure::Loaded, configure::LoadFailure>,
        responder: Option<Responder>,
        is_apply: bool,
        previous_blocked: configure::Blocked,
        previous_hash: Option<Digest>,
    },
    Shutdown,
}

#[allow(dead_code)] // `connection` is informational; stream rules are enforced by the server.
struct ClientEntry {
    kind: ClientKind,
    connection: ConnectionKind,
    out: Outbound,
}

/// Which definition set the actor runs from, and what is on disk.
enum ConfigState {
    NotSetup,
    Accepted {
        set: Arc<ConfigSet>,
        disk_issues: Option<Issues>,
    },
    /// Disk configuration is invalid and no set was accepted in this host lifetime.
    Invalid(Issues),
}

pub struct Actor {
    paths: WorkspacePaths,
    epoch: HostEpoch,
    rx: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<Msg>,
    runner_rx: mpsc::Receiver<(RunId, RunnerEvent)>,
    runner_tx: mpsc::Sender<(RunId, RunnerEvent)>,
    storage: Result<Storage, StorageError>,
    config: ConfigState,
    catalog_revision: CatalogRevision,
    state_revision: StateRevision,
    clients: HashMap<ClientId, ClientEntry>,
    session: Option<Session>,
    runs: HashMap<RunId, ActiveRun>,
    by_action: HashMap<ActionRef, RunId>,
    recent: VecDeque<(RunRecord, SharedLog)>,
    validators: HashMap<Digest, Arc<jsonschema::Validator>>,
    subs: HashMap<SubscriptionId, Sub>,
    event_seq: EventSeq,
    storage_warnings: Vec<Warning>,
    idle_since: Option<Instant>,
    views: views::ViewStore,
    artifacts: artifacts::Artifacts,
    cfg: configure::ConfigCtl,
    schedules: schedule::Schedules,
}

fn load_config(paths: &WorkspacePaths) -> ConfigState {
    if !paths.lyra_dir.join(config::WORKSPACE_FILE).is_file() {
        return ConfigState::NotSetup;
    }
    let local = match config::load_local(&paths.lyra_dir) {
        Ok(l) => l,
        Err(i) => return ConfigState::Invalid(i),
    };
    match config::load_config_dir(&paths.lyra_dir, local.as_ref()) {
        Ok(set) => ConfigState::Accepted {
            set: Arc::new(set),
            disk_issues: None,
        },
        Err(i) => ConfigState::Invalid(i),
    }
}

pub(crate) fn params<P: DeserializeOwned>(p: Map<String, Value>) -> Result<P, RpcError> {
    serde_json::from_value(Value::Object(p))
        .map_err(|e| RpcError::new(RpcError::INVALID_PARAMS, e.to_string()))
}

impl Actor {
    pub fn new(paths: WorkspacePaths, rx: mpsc::Receiver<Msg>, tx: mpsc::Sender<Msg>) -> Self {
        let config = load_config(&paths);
        if let ConfigState::Invalid(i) = &config {
            diag(format!(
                "configuration on disk is invalid: {}",
                i.to_error_info().message
            ));
        }
        let mut storage_warnings = Vec::new();
        let storage = match Storage::open(&paths) {
            Ok((s, report)) => {
                if !report.interrupted.is_empty() {
                    diag(format!(
                        "{} run(s) were active when the previous host stopped; marked interrupted",
                        report.interrupted.len()
                    ));
                }
                Ok(s)
            }
            Err(e) => {
                diag(format!("storage unavailable: {e}"));
                storage_warnings.push(Warning {
                    code: e.to_error_info().code,
                    message: e.to_string(),
                    subject: None,
                });
                Err(e)
            }
        };
        let (runner_tx, runner_rx) = mpsc::channel(4096);
        let paths_for_cfg = paths.clone();
        Self {
            paths,
            epoch: HostEpoch::random(),
            rx,
            tx,
            runner_rx,
            runner_tx,
            storage,
            config,
            catalog_revision: CatalogRevision::ZERO,
            state_revision: StateRevision::ZERO,
            clients: HashMap::new(),
            session: None,
            runs: HashMap::new(),
            by_action: HashMap::new(),
            recent: VecDeque::new(),
            validators: HashMap::new(),
            subs: HashMap::new(),
            event_seq: EventSeq::ZERO,
            storage_warnings,
            idle_since: Some(Instant::now()),
            views: views::ViewStore::default(),
            artifacts: artifacts::Artifacts::default(),
            cfg: configure::ConfigCtl::new(&paths_for_cfg),
            schedules: schedule::Schedules::default(),
        }
    }

    pub fn epoch(&self) -> &HostEpoch {
        &self.epoch
    }

    async fn init_catalog_revision(&mut self) {
        let hash = match &self.config {
            ConfigState::Accepted { set, .. } => Some(set.set_hash.clone()),
            _ => None,
        };
        let Ok(storage) = &self.storage else {
            self.catalog_revision =
                CatalogRevision::new(u64::from(hash.is_some())).unwrap_or_default();
            return;
        };
        let result = match hash {
            Some(h) => storage.accept_catalog(h).await,
            None => storage.catalog().await.map(|(r, _)| r),
        };
        match result {
            Ok(r) => self.catalog_revision = r,
            Err(e) => self.storage_warning(&e),
        }
    }

    pub(crate) fn storage_warning(&mut self, e: &StorageError) {
        diag(format!("storage: {e}"));
        let w = Warning {
            code: e.to_error_info().code,
            message: e.to_string(),
            subject: None,
        };
        if !self.storage_warnings.iter().any(|x| x.message == w.message) {
            self.storage_warnings.push(w);
            if self.storage_warnings.len() > 8 {
                self.storage_warnings.remove(0);
            }
        }
    }

    fn workspace_ref(&self) -> WorkspaceRef {
        WorkspaceRef {
            id: self.paths.id.clone(),
            root: self.paths.root.clone(),
        }
    }

    pub(crate) fn ctx(&self) -> ReplyContext {
        ReplyContext {
            workspace: Some(self.workspace_ref()),
            host_epoch: Some(self.epoch.clone()),
            catalog_revision: Some(self.catalog_revision),
            state_revision: Some(self.state_revision),
        }
    }

    pub(crate) fn ok<T: Serialize>(&self, data: T, meta: ReplyMeta) -> Handled {
        reply_ok(self.ctx(), data, meta)
    }

    pub(crate) fn fail(&self, error: ErrorInfo) -> Handled {
        reply_fail(self.ctx(), error)
    }

    fn busy(&self) -> bool {
        self.session.is_some() || !self.runs.is_empty()
    }

    pub async fn run(mut self) {
        self.init_catalog_revision().await;
        self.init_views().await;
        self.load_schedules().await;
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                biased;
                msg = self.rx.recv() => match msg {
                    Some(Msg::Shutdown) | None => {
                        self.shutdown().await;
                        self.flush_views_now().await;
                        break;
                    }
                    Some(m) => self.handle(m),
                },
                Some((run_id, ev)) = self.runner_rx.recv() => self.runner_event(run_id, ev),
                Some(()) = self.cfg.fs_rx.recv() => self.fs_changed(),
                _ = tick.tick() => {
                    self.session_tick();
                    self.flush_progress();
                    self.views_tick();
                    self.config_tick();
                    self.schedule_tick();
                    if self.idle_since.is_some_and(|t| t.elapsed() >= IDLE_EXIT) {
                        self.flush_views_now().await;
                        break;
                    }
                }
            }
            self.update_idle();
        }
    }

    /// Stops owned work on SIGTERM, bounded by each run's grace period.
    async fn shutdown(&mut self) {
        self.stop_session(lyra_protocol::run::StopReason::SessionClosed);
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.runs.is_empty() && Instant::now() < deadline {
            tokio::select! {
                Some((run_id, ev)) = self.runner_rx.recv() => self.runner_event(run_id, ev),
                Some(m) = self.rx.recv() => {
                    if matches!(m, Msg::FinalSaved { .. } | Msg::ViewSaved { .. } | Msg::ViewBlock { .. } | Msg::ViewClaimed { .. }) {
                        self.handle(m)
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    fn update_idle(&mut self) {
        let idle = self.clients.is_empty() && !self.busy();
        self.idle_since = match (idle, self.idle_since) {
            (true, Some(t)) => Some(t),
            (true, None) => Some(Instant::now()),
            (false, _) => None,
        };
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Hello { params, out, reply } => {
                let _ = reply.send(self.hello(params, out));
            }
            Msg::Request {
                client,
                id,
                method,
                params,
            } => {
                let Some(out) = self.clients.get(&client).map(|c| c.out.clone()) else {
                    return;
                };
                self.dispatch(&client, method, params, Responder { out, id });
            }
            Msg::Closed { client } => {
                self.clients.remove(&client);
                self.subs.retain(|_, s| s.client != client);
                self.controller_left(&client);
            }
            Msg::Reserved { run_id, result } => self.reserved(run_id, result),
            Msg::FinalSaved { run_id, error } => self.final_saved(run_id, error),
            Msg::ViewBlock { result } => self.view_block(result),
            Msg::ViewClaimed { result } => self.view_claimed(result),
            Msg::ViewSaved {
                view_ref,
                revision,
                error,
            } => self.view_saved(view_ref, revision, error),
            Msg::ScheduleSaved {
                params,
                saved,
                responder,
                ..
            } => self.schedule_saved(params, saved, responder),
            Msg::ConfigDone {
                result,
                responder,
                is_apply,
                previous_blocked,
                previous_hash,
            } => self.config_done(result, responder, is_apply, previous_blocked, previous_hash),
            Msg::Shutdown => {}
        }
    }

    fn hello(&mut self, p: HelloParams, out: Outbound) -> Result<(ClientId, HelloReply), RpcError> {
        let refuse = |msg: String| {
            let info = ErrorInfo::new(ErrorCode::PROTOCOL_MISMATCH, msg.clone())
                .with_next_action(&["lyra", "status"], "Retry after the running host finishes its work and exits, or use the matching lyra build.");
            RpcError::new(RpcError::HANDSHAKE, msg).with_info(info)
        };
        if &p.protocol_hash != schemas::protocol_hash() {
            return Err(refuse(format!(
                "this host runs a different Lyra protocol build (host {} {}, client {})",
                lyra_protocol::VERSION,
                schemas::protocol_hash(),
                p.protocol_hash
            )));
        }
        if p.workspace_id != self.paths.id || p.workspace_root != self.paths.root {
            return Err(refuse(format!(
                "this host serves workspace {} at {}, not {} at {}",
                self.paths.id, self.paths.root, p.workspace_id, p.workspace_root
            )));
        }
        let id = ClientId::random();
        self.clients.insert(
            id.clone(),
            ClientEntry {
                kind: p.client_kind,
                connection: p.connection_kind,
                out,
            },
        );
        Ok((
            id.clone(),
            HelloReply {
                api: Api1,
                protocol_hash: schemas::protocol_hash().clone(),
                host_epoch: self.epoch.clone(),
                client_id: id,
                workspace: self.workspace_ref(),
                catalog_revision: self.catalog_revision,
                state_revision: self.state_revision,
            },
        ))
    }

    pub(crate) fn client_kind(&self, client: &ClientId) -> ClientKind {
        self.clients.get(client).map_or(ClientKind::Cli, |c| c.kind)
    }

    fn dispatch(&mut self, client: &ClientId, method: Method, p: Map<String, Value>, r: Responder) {
        macro_rules! parse {
            ($p:expr) => {
                match params($p) {
                    Ok(v) => v,
                    Err(e) => return r.send(Err(e)),
                }
            };
        }
        match method {
            Method::WorkspaceStatus => {
                let _: Empty = parse!(p);
                r.send(self.ok(self.status_data(), ReplyMeta::default()))
            }
            Method::CatalogListM => {
                let res = self.catalog(parse!(p));
                r.send(res)
            }
            Method::ItemDescribe => {
                let res = self.describe(parse!(p));
                r.send(res)
            }
            Method::PathsGet => {
                let _: Empty = parse!(p);
                r.send(self.ok(self.paths.to_data(), ReplyMeta::default()))
            }
            Method::SessionAttach => self.session_attach(client, parse!(p), r),
            Method::SessionDetach => {
                let _: Empty = parse!(p);
                self.session_detach(client, r)
            }
            Method::SessionOpen => self.session_open(client, parse!(p), r),
            Method::SessionKeep => self.session_keep(parse!(p), r),
            Method::SessionStop => {
                let _: Empty = parse!(p);
                self.session_stop_request(r)
            }
            Method::ActionInvoke => self.invoke(client, parse!(p), r),
            Method::ActionExec => self.exec(client, parse!(p), r),
            Method::RunStop => self.run_stop(parse!(p), r),
            Method::RunGet => self.run_get(parse!(p), r),
            Method::RunListM => self.run_list(parse!(p), r),
            Method::LogRead => self.log_read(parse!(p), r),
            Method::ViewRead => self.view_read(parse!(p), r),
            Method::ViewPublish => self.view_publish(parse!(p), r),
            Method::ViewAction => self.view_action(client, parse!(p), r),
            Method::ArtifactList => self.artifact_list(parse!(p), r),
            Method::ArtifactRead => self.artifact_read(parse!(p), r),
            Method::ScheduleSet => self.schedule_set(client, parse!(p), r),
            Method::ConfigApply => self.config_apply(parse!(p), r),
            Method::ConfigReload => {
                let _: Empty = parse!(p);
                self.config_reload(Some(r))
            }
            Method::StreamSubscribe => self.subscribe(client, parse!(p), r),
            Method::StreamUnsubscribe => self.unsubscribe(client, parse!(p), r),
            other => r.send(Err(RpcError::new(
                RpcError::METHOD_NOT_FOUND,
                format!("`{}` is not available in this build", other.name()),
            ))),
        }
    }

    fn config_warnings(&self) -> Vec<Warning> {
        match &self.config {
            ConfigState::NotSetup => vec![],
            ConfigState::Accepted {
                disk_issues: None, ..
            } => vec![],
            ConfigState::Accepted {
                disk_issues: Some(i),
                ..
            }
            | ConfigState::Invalid(i) => {
                let info = i.to_error_info();
                vec![Warning {
                    code: ErrorCode::SCHEMA_INVALID,
                    message: format!("configuration on disk is not in effect: {}", info.message),
                    subject: None,
                }]
            }
        }
    }

    pub(crate) fn status_data(&self) -> StatusData {
        let mut runs: Vec<_> = self
            .runs
            .values()
            .map(|r| lyra_protocol::run::RunSummary::from(&r.record))
            .collect();
        runs.sort_by(|a, b| {
            a.started_at
                .cmp(&b.started_at)
                .then(a.run_id.cmp(&b.run_id))
        });
        let mut storage_warnings = self.storage_warnings.clone();
        for r in self.runs.values() {
            if let Ok(log) = r.log.lock()
                && let Some(e) = &log.write_error
            {
                storage_warnings.push(Warning {
                    code: ErrorCode::STORAGE_UNAVAILABLE,
                    message: format!(
                        "log writes failed ({} record(s) not saved): {e}",
                        log.dropped
                    ),
                    subject: Some(r.record.run_id.to_string()),
                });
            }
        }
        let mut config_warnings = self.config_warnings();
        config_warnings.extend(self.retiring_warnings());
        StatusData {
            session: self.session_info(),
            runs,
            storage_warnings,
            config_warnings,
            schedules: self.schedule_data(),
        }
    }

    pub(crate) fn accepted(&self) -> Result<Arc<ConfigSet>, ErrorInfo> {
        match &self.config {
            ConfigState::Accepted { set, .. } => Ok(set.clone()),
            ConfigState::NotSetup => Err(ErrorInfo::new(
                ErrorCode::NOT_SETUP,
                "this workspace has no .lyra/workspace.json yet",
            )
            .with_next_action(
                &["lyra", "setup", "--json"],
                "Discover project facts, then let your agent create plugins with the lyra skill.",
            )),
            ConfigState::Invalid(i) => Err(i.to_error_info()),
        }
    }

    fn catalog(&self, p: CatalogListParams) -> Handled {
        let set = match self.accepted() {
            Ok(s) => s,
            Err(e) => return self.fail(e),
        };
        if p.if_revision == Some(self.catalog_revision) {
            let meta = ReplyMeta {
                not_modified: true,
                ..ReplyMeta::default()
            };
            return self.ok(CatalogList { items: vec![] }, meta);
        }
        let limit = p
            .limit
            .map_or(DEFAULT_CATALOG_LIMIT, |l| (l as usize).clamp(1, MAX_LIMIT));
        let query = p.query.as_deref().map(str::to_lowercase);
        let items: Vec<CatalogItem> = set
            .catalog()
            .into_iter()
            .filter(|i| match &query {
                None => true,
                Some(q) => {
                    let hay = format!(
                        "{} {} {} {}",
                        i.item_ref,
                        i.title,
                        i.description,
                        i.tags.join(" ")
                    )
                    .to_lowercase();
                    q.split_whitespace().all(|w| hay.contains(w))
                }
            })
            .collect();
        let truncated = items.len() > limit;
        let meta = ReplyMeta {
            truncated,
            ..ReplyMeta::default()
        };
        self.ok(
            CatalogList {
                items: items.into_iter().take(limit).collect(),
            },
            meta,
        )
    }

    fn describe(&self, p: ItemDescribeParams) -> Handled {
        let set = match self.accepted() {
            Ok(s) => s,
            Err(e) => return self.fail(e),
        };
        let not_found = || {
            ErrorInfo::new(
                ErrorCode::NOT_FOUND,
                format!("no catalog item `{}`", p.item_ref),
            )
        };
        let Some(lp) = set.plugin(&p.item_ref.plugin) else {
            return self.fail(not_found());
        };
        let Some(item) = set.catalog().into_iter().find(|i| i.item_ref == p.item_ref) else {
            return self.fail(not_found());
        };
        let plugin = &lp.plugin;
        let summary = PluginSummary {
            id: plugin.id.clone(),
            name: plugin.name.clone(),
            description: plugin.description.clone(),
            enabled: plugin.enabled,
            docs: plugin.docs.clone(),
        };
        let (action, view, hint) = if let Some(a) = plugin.action(p.item_ref.item.as_str()) {
            let desc = ActionDescription {
                mode: a.mode,
                runner: match a.run {
                    Runner::Command { .. } => "command".into(),
                    Runner::Plugin => "plugin".into(),
                },
                terminal: a.terminal,
                timeout: a.timeout.to_wire(),
                cwd: a.cwd.clone(),
                env_names: a.env.keys().cloned().collect(),
                env_files: a.env_files.clone(),
                effects: a.effects.clone(),
                has_schedule: a.schedule.is_some(),
                write_only_fields: a.input_schema.0.write_only_fields(),
                input_schema: p.include_schema.then(|| a.input_schema.0.as_map().clone()),
                output_schema: if p.include_schema {
                    a.output_schema.as_ref().map(|s| s.0.as_map().clone())
                } else {
                    None
                },
            };
            let verb = match a.mode {
                ActionMode::Task => "run",
                ActionMode::Process => "start",
            };
            let mut hint = vec!["lyra".to_owned(), verb.to_owned(), p.item_ref.to_string()];
            if a.input_schema
                .0
                .as_map()
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|m| !m.is_empty())
            {
                hint.extend(["--input".to_owned(), "input.json".to_owned()]);
            }
            (Some(desc), None, hint)
        } else if let Some(v) = plugin.view(p.item_ref.item.as_str()) {
            let desc = ViewDescription {
                view_kind: v.kind,
                persistence: v.persistence,
                row_actions: v.row_actions.iter().map(|r| r.action.clone()).collect(),
            };
            (
                None,
                Some(desc),
                vec!["lyra".to_owned(), "view".to_owned(), p.item_ref.to_string()],
            )
        } else {
            return self.fail(not_found());
        };
        self.ok(
            ItemDescription {
                item,
                plugin: summary,
                action,
                view,
                invoke_hint: hint,
            },
            ReplyMeta::default(),
        )
    }

    /// Increments `state_revision` and notifies state subscribers.
    pub(crate) fn state_changed(&mut self) {
        if let Some(next) = self.state_revision.next() {
            self.state_revision = next;
        }
        self.broadcast_state();
    }

    pub(crate) fn kinds_of(kinds: &[StreamKind]) -> BTreeSet<StreamKind> {
        kinds.iter().copied().collect()
    }
}

pub(crate) fn reply_ok<T: Serialize>(ctx: ReplyContext, data: T, meta: ReplyMeta) -> Handled {
    serde_json::to_value(PublicReply::success(ctx, data, meta))
        .map_err(|e| RpcError::new(RpcError::INTERNAL_ERROR, e.to_string()))
}

pub(crate) fn reply_fail(ctx: ReplyContext, error: ErrorInfo) -> Handled {
    serde_json::to_value(PublicReply::<Value>::failure(ctx, error))
        .map_err(|e| RpcError::new(RpcError::INTERNAL_ERROR, e.to_string()))
}
