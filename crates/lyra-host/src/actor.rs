//! The workspace actor: the only writer of workspace state (§3.1, §9.4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use lyra_protocol::config::{self, ConfigSet};
use lyra_protocol::error::{ErrorCode, ErrorInfo, Issues};
use lyra_protocol::ids::*;
use lyra_protocol::ipc::*;
use lyra_protocol::manifest::Runner;
use lyra_protocol::paths::WorkspacePaths;
use lyra_protocol::reply::{PublicReply, ReplyContext, ReplyMeta, WorkspaceRef};
use lyra_protocol::schemas;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::diag;
use crate::server::{error_line, result_line};

/// Observe-only hosts exit this long after the last client leaves with no session.
const IDLE_EXIT: Duration = Duration::from_secs(5);
const DEFAULT_CATALOG_LIMIT: usize = 30;
const MAX_LIMIT: usize = 1000;

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
    #[allow(dead_code)] // used by stream subscriptions (LYR-04/07)
    pub fn event(&self, line: Vec<u8>) -> bool {
        self.events.try_send(line).is_ok()
    }
}

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
    Shutdown,
}

#[allow(dead_code)] // kind/connection drive controller and subscription rules (LYR-04/07).
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
    _tx: mpsc::Sender<Msg>,
    config: ConfigState,
    catalog_revision: CatalogRevision,
    state_revision: StateRevision,
    clients: HashMap<ClientId, ClientEntry>,
    idle_since: Option<Instant>,
}

type Handled = Result<Value, RpcError>;

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

fn params<P: DeserializeOwned>(p: Map<String, Value>) -> Result<P, RpcError> {
    serde_json::from_value(Value::Object(p))
        .map_err(|e| RpcError::new(RpcError::INVALID_PARAMS, e.to_string()))
}

impl Actor {
    pub fn new(paths: WorkspacePaths, rx: mpsc::Receiver<Msg>, tx: mpsc::Sender<Msg>) -> Self {
        let config = load_config(&paths);
        let catalog_revision = match &config {
            ConfigState::Accepted { .. } => CatalogRevision::new(1).unwrap_or_default(),
            _ => CatalogRevision::ZERO,
        };
        if let ConfigState::Invalid(i) = &config {
            diag(format!(
                "configuration on disk is invalid: {}",
                i.to_error_info().message
            ));
        }
        Self {
            paths,
            epoch: HostEpoch::random(),
            rx,
            _tx: tx,
            config,
            catalog_revision,
            state_revision: StateRevision::ZERO,
            clients: HashMap::new(),
            idle_since: Some(Instant::now()),
        }
    }

    pub fn epoch(&self) -> &HostEpoch {
        &self.epoch
    }

    fn workspace_ref(&self) -> WorkspaceRef {
        WorkspaceRef {
            id: self.paths.id.clone(),
            root: self.paths.root.clone(),
        }
    }

    fn ctx(&self) -> ReplyContext {
        ReplyContext {
            workspace: Some(self.workspace_ref()),
            host_epoch: Some(self.epoch.clone()),
            catalog_revision: Some(self.catalog_revision),
            state_revision: Some(self.state_revision),
        }
    }

    fn ok<T: Serialize>(&self, data: T, meta: ReplyMeta) -> Handled {
        serde_json::to_value(PublicReply::success(self.ctx(), data, meta))
            .map_err(|e| RpcError::new(RpcError::INTERNAL_ERROR, e.to_string()))
    }

    fn fail(&self, error: ErrorInfo) -> Handled {
        serde_json::to_value(PublicReply::<Value>::failure(self.ctx(), error))
            .map_err(|e| RpcError::new(RpcError::INTERNAL_ERROR, e.to_string()))
    }

    fn has_session(&self) -> bool {
        false
    }

    pub async fn run(mut self) {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                msg = self.rx.recv() => match msg {
                    Some(Msg::Shutdown) | None => break,
                    Some(m) => self.handle(m),
                },
                _ = tick.tick() => {
                    if self.idle_since.is_some_and(|t| t.elapsed() >= IDLE_EXIT) {
                        break;
                    }
                }
            }
        }
    }

    fn update_idle(&mut self) {
        let idle = self.clients.is_empty() && !self.has_session();
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
                let result = self.dispatch(&client, method, params);
                if let Some(c) = self.clients.get(&client) {
                    c.out.respond(match result {
                        Ok(v) => result_line(id, v),
                        Err(e) => error_line(Some(id), e),
                    });
                }
            }
            Msg::Closed { client } => {
                self.clients.remove(&client);
            }
            Msg::Shutdown => {}
        }
        self.update_idle();
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

    fn dispatch(&mut self, client: &ClientId, method: Method, p: Map<String, Value>) -> Handled {
        let _ = client;
        match method {
            Method::WorkspaceStatus => {
                let _: Empty = params(p)?;
                self.ok(self.status_data(), ReplyMeta::default())
            }
            Method::CatalogListM => self.catalog(params(p)?),
            Method::ItemDescribe => self.describe(params(p)?),
            Method::PathsGet => {
                let _: Empty = params(p)?;
                self.ok(self.paths.to_data(), ReplyMeta::default())
            }
            other => Err(RpcError::new(
                RpcError::METHOD_NOT_FOUND,
                format!("`{}` is not available in this build", other.name()),
            )),
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

    fn status_data(&self) -> StatusData {
        StatusData {
            session: None,
            runs: vec![],
            storage_warnings: vec![],
            config_warnings: self.config_warnings(),
        }
    }

    fn accepted(&self) -> Result<&Arc<ConfigSet>, ErrorInfo> {
        match &self.config {
            ConfigState::Accepted { set, .. } => Ok(set),
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
                lyra_protocol::manifest::ActionMode::Task => "run",
                lyra_protocol::manifest::ActionMode::Process => "start",
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
}
