//! Presentation state and the single key router (§12.3). The host owns every fact; this
//! state is a projection of `state`/`log` events plus replies, and every action goes through
//! the typed client. Footer and key handling read the same [`App::bindings`] set.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mira_protocol::error::ErrorInfo;
use mira_protocol::ids::{ActionId, ActionRef, Digest, ItemRef, RunId, SessionId, ViewRef};
use mira_protocol::ipc::*;
use mira_protocol::manifest::{ActionMode, JsonObject, ViewKind};
use mira_protocol::run::{ExitInfo, Lifecycle, RunRecord, RunResult, RunSummary};
use mira_protocol::time::Timestamp;
use serde_json::{Map, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::clip;
use crate::cmdbar;
use crate::form::{self, Form, Outcome};
use crate::ipc::{Control, Event, LogChunk, Read, Tx};
use crate::logs::LogPane;
use crate::terminal::Terminals;
use crate::views::ViewPane;

const MAX_PANES: usize = 8;
/// Errors stay this long; other notices are transient and go sooner.
const NOTICE_SECS: u64 = 8;
const INFO_SECS: u64 = 4;
/// The header's git branch is read again at most this often (or on a focus change).
const BRANCH_SECS: u64 = 3;
/// Open views whose metadata can still change (a save in progress, or a producing run
/// that ended) are read again at most this often.
const VIEW_POLL_MS: u128 = 1000;

pub struct Item {
    pub action_ref: ActionRef,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub mode: ActionMode,
    pub enabled: bool,
    pub definition_hash: Digest,
}

pub struct ViewItem {
    pub view_ref: ViewRef,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub kind: ViewKind,
}

/// One row of the tool list: an action or a view of some plugin.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Action(usize),
    View(usize),
}

pub enum Inputs {
    Loading,
    /// No input properties: runs directly.
    Free,
    /// The input schema; a form collects the values.
    Form(JsonObject),
    Unknown(String),
}

/// The tabs of the main pane for an action. `History` shows while the history list is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Logs,
    History,
    Output,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Logs, Tab::History, Tab::Output];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Logs => "Logs",
            Tab::History => "History",
            Tab::Output => "Output",
        }
    }

    fn index(self) -> usize {
        match self {
            Tab::Logs => 0,
            Tab::History => 1,
            Tab::Output => 2,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Logs,
}

pub enum Modal {
    None,
    Search {
        logs: bool,
        text: String,
        prev_filter: String,
    },
    Form(Box<Form>),
    /// The `:` command bar.
    Command {
        text: String,
        error: Option<String>,
    },
    /// Output of a command-bar command.
    Output(Box<cmdbar::Output>),
    /// Choose which row action to run on the selected table row.
    RowAction {
        view_ref: ViewRef,
        choices: Vec<ActionId>,
        index: usize,
    },
    /// The newest runs of one action; `runs` is `None` while the host answers.
    History {
        action_ref: ActionRef,
        runs: Option<Vec<RunRecord>>,
        error: Option<String>,
        index: usize,
    },
    /// `top` is the first shown line; `max` the largest `top` (0 when all lines fit),
    /// which drawing sets.
    Help {
        top: usize,
        max: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intent {
    Start,
    Stop,
    Restart,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Open,
    Toggle,
    Restart,
    Focus,
    Search,
    Quit,
    Keep,
    Left,
    Right,
    Wrap,
    Copy,
    Select,
    Escape,
    NextMatch,
    PrevMatch,
    Help,
    /// Attach to the selected PTY run (or retry taking its input).
    Attach,
    /// Leave the terminal view.
    Detach,
    /// Keys go to the attached program (footer entry only).
    Forward,
    Command,
    Schedule,
    Mouse,
    CopyAll,
    /// Open the run history of the selected action.
    History,
    /// Switch the main pane tab (`[` and `]`).
    NextTab,
    PrevTab,
    /// Go to one tab by number (`1`, `2`, `3`).
    GoTab(u8),
}

pub struct Binding {
    pub keys: &'static str,
    pub label: String,
    pub cmd: Cmd,
    pub footer: bool,
}

pub(crate) fn bind(keys: &'static str, label: impl Into<String>, cmd: Cmd) -> Binding {
    Binding {
        keys,
        label: label.into(),
        cmd,
        footer: true,
    }
}

fn hidden(keys: &'static str, label: impl Into<String>, cmd: Cmd) -> Binding {
    Binding {
        footer: false,
        ..bind(keys, label, cmd)
    }
}

pub struct LastRun {
    pub run_id: RunId,
    pub lifecycle: Lifecycle,
    pub exit: Option<ExitInfo>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    /// The structured result of the run, once its final record is read.
    pub result: Option<RunResult>,
}

pub struct Notice {
    pub text: String,
    pub error: bool,
    at: Instant,
    /// The run an info notice is about; it goes when that run changes state or ends, or
    /// when another item is selected.
    about: Option<(ActionRef, RunId)>,
    /// The run's [`stage`] when the notice was last checked.
    seen: Option<u8>,
}

pub enum Quit {
    Normal(Option<&'static str>),
    Kept(Option<Timestamp>),
}

enum OpenKind {
    Start,
    Logs,
    NeedsInput,
    Form,
}

pub struct Io {
    pub control: UnboundedSender<Control>,
    pub read: UnboundedSender<Read>,
    pub events: Tx,
    pub paths: mira_protocol::paths::WorkspacePaths,
}

pub struct App {
    pub root: String,
    /// `name` from `.mira/workspace.json`, for the header.
    pub workspace_name: Option<String>,
    /// The workspace's git branch (or short commit); `None` outside a git repository.
    pub branch: Option<String>,
    branch_at: Instant,
    view_poll_at: Instant,
    /// Actions whose log pane shows an older run chosen in the history list.
    pub viewing: HashMap<ActionRef, RunRecord>,
    pub offset: time::UtcOffset,
    pub items: Vec<Item>,
    pub views: Vec<ViewItem>,
    plugin_order: Vec<String>,
    pub view_panes: HashMap<ViewRef, ViewPane>,
    pub schedules: Vec<ScheduleData>,
    /// Last visible (never write-only) form values per action, to prefill the next form.
    last_inputs: HashMap<ActionRef, Map<String, Value>>,
    /// Application mouse mode; off keeps the terminal's own selection.
    pub mouse: bool,
    /// A mouse-mode change the render loop still has to apply.
    pub mouse_changed: Option<bool>,
    status_at: Option<Instant>,
    /// Actions whose description declares a schedule.
    scheduled: std::collections::HashSet<ActionRef>,
    pub catalog_error: Option<ErrorInfo>,
    pub filter: String,
    pub visible: Vec<Entry>,
    pub selected: usize,
    pub list_offset: usize,
    pub focus: Focus,
    pub modal: Modal,
    pub session: Option<SessionInfo>,
    /// The session this TUI is a controller of.
    pub attached_to: Option<SessionId>,
    pub active: HashMap<ActionRef, RunSummary>,
    pub adhoc_runs: usize,
    pub storage_warnings: Vec<Warning>,
    pub config_warnings: Vec<Warning>,
    pub last: HashMap<ActionRef, LastRun>,
    pub pending: HashMap<ActionRef, Intent>,
    restart_after: HashMap<ActionRef, (RunId, JsonObject)>,
    pub inputs: HashMap<ActionRef, (Digest, Inputs)>,
    queued: Option<(ActionRef, Intent)>,
    pub panes: HashMap<ActionRef, LogPane>,
    pane_order: VecDeque<ActionRef>,
    pub log_query: Option<String>,
    pub notice: Option<Notice>,
    pub stream_issue: Option<String>,
    pub control_lost: Option<String>,
    pub keeping: bool,
    pub quit: Option<Quit>,
    pub narrow: bool,
    /// The terminal is wide enough for the right rail; drawing sets it.
    pub wide: bool,
    /// The main pane tab for actions (`Logs` or `Output`; `History` is the open list).
    pub tab: Tab,
    /// First shown line of the Output tab.
    pub output_top: usize,
    /// The newest runs per action, for the right rail and a quick history tab.
    pub recent: HashMap<ActionRef, Vec<RunRecord>>,
    /// The run state each `recent` entry was requested for; a change reads it again.
    recent_key: HashMap<ActionRef, (Option<RunId>, u8, bool)>,
    /// The PTY attach view (§13).
    pub term: Terminals,
    io: Io,
}

impl App {
    pub fn new(root: String, offset: time::UtcOffset, io: Io) -> Self {
        let branch = crate::git::head_label(std::path::Path::new(&root));
        Self {
            workspace_name: workspace_name(&root),
            branch,
            branch_at: Instant::now(),
            view_poll_at: Instant::now(),
            viewing: HashMap::new(),
            root,
            offset,
            items: Vec::new(),
            views: Vec::new(),
            plugin_order: Vec::new(),
            view_panes: HashMap::new(),
            schedules: Vec::new(),
            last_inputs: HashMap::new(),
            mouse: false,
            mouse_changed: None,
            status_at: None,
            scheduled: Default::default(),
            catalog_error: None,
            filter: String::new(),
            visible: Vec::new(),
            selected: 0,
            list_offset: 0,
            focus: Focus::List,
            modal: Modal::None,
            session: None,
            attached_to: None,
            active: HashMap::new(),
            adhoc_runs: 0,
            storage_warnings: Vec::new(),
            config_warnings: Vec::new(),
            last: HashMap::new(),
            pending: HashMap::new(),
            restart_after: HashMap::new(),
            inputs: HashMap::new(),
            queued: None,
            panes: HashMap::new(),
            pane_order: VecDeque::new(),
            log_query: None,
            notice: None,
            stream_issue: None,
            control_lost: None,
            keeping: false,
            quit: None,
            narrow: false,
            wide: false,
            tab: Tab::Logs,
            output_top: 0,
            recent: HashMap::new(),
            recent_key: HashMap::new(),
            term: Terminals::new(io.paths.clone()),
            io,
        }
    }

    // ----- projection updates -------------------------------------------------------

    pub fn set_attached(&mut self, session: Option<SessionInfo>) {
        self.attached_to = session.as_ref().map(|s| s.id.clone());
        self.session = session;
    }

    pub fn set_catalog(&mut self, list: CatalogList) {
        let keep = self.selected_key();
        let mut order: Vec<String> = Vec::new();
        self.items.clear();
        self.views.clear();
        for i in list.items {
            let p = i.item_ref.plugin.to_string();
            if !order.contains(&p) {
                order.push(p);
            }
            match i.item {
                CatalogItemKind::Action { mode } => self.items.push(Item {
                    action_ref: i.item_ref.as_action(),
                    title: i.title,
                    description: i.description,
                    tags: i.tags,
                    mode,
                    enabled: i.enabled,
                    definition_hash: i.definition_hash,
                }),
                CatalogItemKind::View { view_kind } => self.views.push(ViewItem {
                    view_ref: i.item_ref.as_view(),
                    title: i.title,
                    description: i.description,
                    tags: i.tags,
                    kind: view_kind,
                }),
            }
        }
        self.plugin_order = order;
        // The workspace file may have changed with the catalog.
        self.workspace_name = workspace_name(&self.root);
        // Views that no longer exist lose their panel; the others read again.
        let known: Vec<ViewRef> = self.views.iter().map(|v| v.view_ref.clone()).collect();
        self.view_panes.retain(|r, _| known.contains(r));
        let refs: Vec<ViewRef> = self.view_panes.keys().cloned().collect();
        for r in refs {
            self.load_view(&r);
        }
        self.catalog_error = None;
        self.refilter(keep);
        self.on_select();
    }

    fn apply_runs(&mut self, session: Option<SessionInfo>, runs: Vec<RunSummary>) {
        let old = std::mem::take(&mut self.active);
        self.adhoc_runs = 0;
        for r in runs {
            match r.action_ref.clone() {
                Some(a) => {
                    // A new run of this action replaces a historical run in its pane.
                    if old.get(&a).is_none_or(|o| o.run_id != r.run_id) {
                        self.viewing.remove(&a);
                    }
                    self.active.insert(a, r);
                }
                None => self.adhoc_runs += 1,
            }
        }
        for (a, r) in old {
            if self.active.get(&a).is_some_and(|n| n.run_id == r.run_id) {
                continue;
            }
            // Views this run produced are no longer current: read their metadata again.
            let produced: Vec<ViewRef> = self
                .view_panes
                .iter()
                .filter(|(_, p)| {
                    p.meta
                        .as_ref()
                        .is_some_and(|m| m.source_run_id.as_ref() == Some(&r.run_id))
                })
                .map(|(v, _)| v.clone())
                .collect();
            for v in produced {
                self.load_view(&v);
            }
            // The run ended: fetch its final record for the outcome.
            self.last.insert(
                a.clone(),
                LastRun {
                    run_id: r.run_id.clone(),
                    lifecycle: r.lifecycle,
                    exit: None,
                    started_at: Some(r.started_at),
                    ended_at: None,
                    result: None,
                },
            );
            let _ = self.io.read.send(Read::RunGet(r.run_id.clone()));
            if self
                .restart_after
                .get(&a)
                .is_some_and(|(id, _)| *id == r.run_id)
                && let Some((_, input)) = self.restart_after.remove(&a)
            {
                self.invoke(a, input);
            }
        }
        self.session = session;
        if let Some(a) = self.selected_ref() {
            self.sync_pane(&a);
        }
        self.settle_notice();
        self.request_status();
    }

    /// Schedules come only with full status; read it at most once a second.
    fn request_status(&mut self) {
        if self
            .status_at
            .is_none_or(|t| t.elapsed().as_millis() >= 1000)
        {
            self.status_at = Some(Instant::now());
            let _ = self.io.read.send(Read::Status);
        }
    }

    /// The action declares an interval schedule (switched on or not yet).
    pub fn has_schedule(&self, a: &ActionRef) -> bool {
        self.scheduled.contains(a) || self.schedule_of(a).is_some()
    }

    pub fn schedule_of(&self, a: &ActionRef) -> Option<&ScheduleData> {
        self.schedules.iter().find(|s| &s.action_ref == a)
    }

    fn load_view(&mut self, view_ref: &ViewRef) {
        let kind = self
            .views
            .iter()
            .find(|v| &v.view_ref == view_ref)
            .map(|v| v.kind);
        let Some(kind) = kind else { return };
        let p = self
            .view_panes
            .entry(view_ref.clone())
            .or_insert_with(|| ViewPane::new(kind));
        if p.loading {
            p.reload = true;
            return;
        }
        p.loading = true;
        let _ = self.io.read.send(Read::View(view_ref.clone()));
    }

    /// Reads the git branch again when it is older than [`BRANCH_SECS`] or `force` is set.
    fn refresh_branch(&mut self, force: bool) {
        if force || self.branch_at.elapsed().as_secs() >= BRANCH_SECS {
            self.branch_at = Instant::now();
            self.branch = crate::git::head_label(std::path::Path::new(&self.root));
        }
    }

    pub fn handle(&mut self, ev: Event) {
        // A busy event stream can starve the idle tick; its checks are rate-limited.
        self.tick();
        match ev {
            Event::Input(crossterm::event::Event::Key(k)) => self.key(k),
            Event::Input(crossterm::event::Event::Paste(t)) => {
                if self.term.is_open() {
                    self.term.paste(t)
                } else {
                    self.paste(&t)
                }
            }
            Event::Input(crossterm::event::Event::Mouse(m)) => self.mouse_event(m),
            Event::Input(_) => {}
            Event::Terminal(m) => self.term.handle(m),
            Event::Frame(frame) => self.frame(*frame),
            Event::StreamReset => {
                self.info("event stream reset (this TUI fell behind); state and log tail reloaded");
                self.reload_selected_tail();
            }
            Event::StreamDown(m) => self.stream_issue = Some(format!("event stream lost: {m}")),
            Event::Invoked(a, res, joined) => {
                self.pending.remove(&a);
                if let Modal::Form(f) = &mut self.modal
                    && f.action_ref == a
                {
                    match &res {
                        Ok(_) => self.modal = Modal::None,
                        Err(e) => {
                            f.apply_error(e);
                            self.restart_after.remove(&a);
                            return;
                        }
                    }
                }
                if joined.is_some() {
                    self.attached_to = joined;
                }
                match res {
                    Ok(acc) => {
                        let what = if acc.reused {
                            format!("{a} is already running ({})", acc.run_id)
                        } else {
                            format!("started {a} ({})", acc.run_id)
                        };
                        self.info_run(&a, &acc.run_id, what);
                        self.viewing.remove(&a);
                        if let Some(p) = self.panes.get_mut(&a)
                            && p.run_id.as_ref() != Some(&acc.run_id)
                        {
                            p.reset(Some(acc.run_id.clone()));
                            p.loading = true;
                            let _ = self.io.read.send(Read::Tail(a.clone(), Some(acc.run_id)));
                        }
                    }
                    Err(e) => {
                        self.restart_after.remove(&a);
                        self.error_info(&format!("{a} did not start"), &e);
                    }
                }
            }
            Event::Stopped(a, res) => {
                self.pending.remove(&a);
                match res {
                    Ok(s) => {
                        let what = format!("stopping {a} ({})", s.run_id);
                        self.info_run(&a, &s.run_id, what)
                    }
                    Err(e) => {
                        self.restart_after.remove(&a);
                        self.error_info(&format!("{a} did not stop"), &e);
                    }
                }
            }
            Event::Kept(res) => {
                self.keeping = false;
                match res {
                    Ok(d) => self.quit = Some(Quit::Kept(d.session.and_then(|s| s.expires_at))),
                    Err(e) => self.error_info("could not keep the session", &e),
                }
            }
            Event::Described(a, res) => {
                self.term.note_described(&a, &res);
                let hash = res
                    .as_ref()
                    .map(|d| d.item.definition_hash.clone())
                    .ok()
                    .or_else(|| self.item(&a).map(|i| i.definition_hash.clone()));
                let inputs = match res {
                    Ok(d) => {
                        if d.action.as_ref().is_some_and(|x| x.has_schedule) {
                            self.scheduled.insert(a.clone());
                        }
                        inputs_of(d.action.as_ref().and_then(|x| x.input_schema.as_ref()))
                    }
                    Err(e) => Inputs::Unknown(e.message),
                };
                if let Some(h) = hash {
                    self.inputs.insert(a.clone(), (h, inputs));
                }
                if let Some((qa, intent)) = self.queued.take() {
                    if qa == a {
                        self.intent(&a, intent);
                    } else {
                        self.queued = Some((qa, intent));
                    }
                }
            }
            Event::Tail(a, res) => self.tail(&a, res),
            Event::Older(run_id, res) => {
                if let Some(p) = self
                    .panes
                    .values_mut()
                    .find(|p| p.run_id.as_ref() == Some(&run_id))
                {
                    match res {
                        Ok(LogChunk { page, older_cursor }) => p.apply_older(page, older_cursor),
                        Err(e) => {
                            p.loading_older = false;
                            p.older_cursor = None;
                            p.error = Some(e.message);
                        }
                    }
                }
            }
            Event::Run(Ok(rec)) => self.record(*rec),
            Event::Run(Err(_)) => {}
            Event::Recent(Ok(list)) => {
                for rec in list.runs {
                    if let Some(a) = rec.action_ref.clone()
                        && !self.last.contains_key(&a)
                        && !rec.lifecycle.is_active()
                    {
                        self.record(rec);
                    }
                }
            }
            Event::Recent(Err(_)) => {}
            Event::History(a, res) => {
                if let Ok(list) = &res {
                    self.recent.insert(a.clone(), list.runs.clone());
                }
                if let Modal::History {
                    action_ref,
                    runs,
                    error,
                    index,
                } = &mut self.modal
                    && *action_ref == a
                {
                    match res {
                        Ok(list) => {
                            // Keep the choice on the run whose logs the pane shows.
                            let shown = self.panes.get(&a).and_then(|p| p.run_id.clone());
                            *index = list
                                .runs
                                .iter()
                                .position(|r| Some(&r.run_id) == shown.as_ref())
                                .unwrap_or(0);
                            *runs = Some(list.runs);
                        }
                        Err(e) => *error = Some(format!("[{}] {}", e.code, e.message)),
                    }
                }
            }
            Event::Catalog(Ok(list)) => self.set_catalog(list),
            Event::Catalog(Err(e)) => self.catalog_error = Some(e),
            Event::CatalogChanged => {
                self.inputs.clear();
                let _ = self.io.read.send(Read::Catalog);
            }
            Event::ConnectionLost(which, m) => {
                if which == "control" {
                    self.control_lost = Some(m.clone());
                }
                self.error(format!("host {which} connection lost: {m}"));
            }
            Event::Copied(Ok(m)) => self.info(m),
            Event::Copied(Err(m)) => self.error(m),
            Event::Signal(name) => self.quit = Some(Quit::Normal(Some(name))),
            Event::View(r, res) => {
                let Some(p) = self.view_panes.get_mut(&r) else {
                    return;
                };
                p.loading = false;
                match res {
                    Ok(load) => p.apply(load.snapshot, load.truncated),
                    Err(e) => p.error = Some(format!("[{}] {}", e.code, e.message)),
                }
                if std::mem::take(&mut p.reload) {
                    self.load_view(&r);
                }
            }
            Event::ViewDescribed(r, res) => {
                if let (Some(p), Ok(d)) = (self.view_panes.get_mut(&r), res) {
                    p.row_actions = d.view.map(|v| v.row_actions).unwrap_or_default();
                }
            }
            Event::ViewActed(r, action, res) => match res {
                Ok(acc) => self.info(format!(
                    "started {}.{action} for the selected row ({}); select that action to see its logs",
                    r.plugin, acc.run_id
                )),
                Err(e) if e.code == mira_protocol::ErrorCode::VIEW_CHANGED => {
                    if let Some(p) = self.view_panes.get_mut(&r) {
                        p.accept_current();
                    }
                    self.load_view(&r);
                    self.error(format!(
                        "VIEW_CHANGED, nothing was run: {} Showing the current revision; check the row, then press Enter again.",
                        e.message
                    ));
                }
                Err(e) => self.error_info(&format!("{}.{action} did not start", r.plugin), &e),
            },
            Event::ScheduleSet(a, res) => {
                self.pending.remove(&a);
                match res {
                    Ok(d) => {
                        self.info(format!(
                            "schedule for {a} is {}",
                            if d.enabled { "on" } else { "off" }
                        ));
                        self.status_at = None;
                        self.request_status();
                    }
                    Err(e) => self.error_info(&format!("could not change the {a} schedule"), &e),
                }
            }
            Event::Status(Ok(st)) => {
                self.schedules = st.schedules;
                self.config_warnings = st.config_warnings;
            }
            Event::Status(Err(_)) => {}
            Event::CommandDone(out) => {
                if matches!(self.modal, Modal::Command { .. } | Modal::None) {
                    self.modal = Modal::Output(Box::new(out));
                }
            }
        }
    }

    fn frame(&mut self, frame: StreamFrame) {
        match frame.event {
            StreamEvent::Ready { .. } => {}
            StreamEvent::Snapshot(s) => {
                if self.stream_issue.take().is_some() {
                    self.reload_selected_tail();
                }
                self.storage_warnings = s.storage_warnings;
                self.config_warnings = s.config_warnings;
                self.schedules = s.schedules;
                self.apply_runs(s.session, s.runs);
                // Views may have changed while the stream was down.
                let refs: Vec<ViewRef> = self.view_panes.keys().cloned().collect();
                for r in refs {
                    self.load_view(&r);
                }
            }
            StreamEvent::State {
                session,
                runs,
                storage_warnings,
                ..
            } => {
                self.storage_warnings = storage_warnings;
                self.apply_runs(session, runs);
            }
            StreamEvent::Log { run_id, records } => {
                for p in self.panes.values_mut() {
                    p.append(&run_id, &records);
                }
            }
            StreamEvent::View {
                view_ref,
                view_revision,
            } => {
                // Only opened views keep a panel; others read when they are selected.
                if self
                    .view_panes
                    .get(&view_ref)
                    .is_some_and(|p| p.revision() != Some(view_revision))
                {
                    self.load_view(&view_ref);
                }
            }
            StreamEvent::Gap {
                dropped_records, ..
            } => {
                self.error(format!(
                    "event stream gap ({} records); reloading the log tail",
                    dropped_records.unwrap_or(0)
                ));
                self.reload_selected_tail();
            }
            _ => {}
        }
    }

    fn record(&mut self, rec: RunRecord) {
        let Some(a) = rec.action_ref.clone() else {
            return;
        };
        if self.active.get(&a).is_some_and(|r| r.run_id == rec.run_id) {
            return;
        }
        // Keep the newest finished run per action.
        if let Some(l) = self.last.get(&a)
            && l.run_id != rec.run_id
            && l.ended_at
                .is_some_and(|t| rec.ended_at.is_none_or(|e| e < t))
        {
            return;
        }
        self.last.insert(
            a,
            LastRun {
                run_id: rec.run_id,
                lifecycle: rec.lifecycle,
                exit: rec.exit,
                started_at: Some(rec.started_at),
                ended_at: rec.ended_at,
                result: rec.result,
            },
        );
    }

    fn tail(&mut self, a: &ActionRef, res: Result<LogChunk, ErrorInfo>) {
        let Some(p) = self.panes.get_mut(a) else {
            return;
        };
        match res {
            Ok(LogChunk { page, older_cursor }) => {
                if p.run_id.as_ref().is_some_and(|r| r != &page.run_id) {
                    p.loading = false;
                    return;
                }
                p.apply_tail(page, older_cursor);
            }
            Err(e) => {
                p.loading = false;
                if e.code != mira_protocol::ErrorCode::NOT_FOUND {
                    p.error = Some(e.message);
                }
            }
        }
    }

    fn reload_selected_tail(&mut self) {
        let Some(a) = self.selected_ref() else {
            return;
        };
        if let Some(p) = self.panes.get_mut(&a) {
            p.loading = true;
            let _ = self.io.read.send(Read::Tail(a.clone(), p.run_id.clone()));
        }
    }

    pub fn tick(&mut self) {
        self.refresh_branch(false);
        self.poll_views();
        if self.notice.as_ref().is_some_and(|n| {
            n.at.elapsed().as_secs() >= if n.error { NOTICE_SECS } else { INFO_SECS }
        }) {
            self.notice = None;
        }
    }

    /// Reads open views again while their freshness or durability can still change without
    /// a new revision: a save in progress, or "current" data whose producing run ended.
    fn poll_views(&mut self) {
        if self.view_poll_at.elapsed().as_millis() < VIEW_POLL_MS {
            return;
        }
        self.view_poll_at = Instant::now();
        let running: Vec<&RunId> = self.active.values().map(|r| &r.run_id).collect();
        let due: Vec<ViewRef> = self
            .view_panes
            .iter()
            .filter(|(_, p)| !p.loading && p.meta.as_ref().is_some_and(|m| m.may_change(&running)))
            .map(|(v, _)| v.clone())
            .collect();
        for v in due {
            self.load_view(&v);
        }
    }

    // ----- notices ------------------------------------------------------------------

    pub fn info(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: false,
            at: Instant::now(),
            about: None,
            seen: None,
        });
    }

    /// An info notice about one run: it goes as soon as the run moves on.
    fn info_run(&mut self, a: &ActionRef, run_id: &RunId, text: impl Into<String>) {
        self.info(text);
        if let Some(n) = &mut self.notice {
            n.about = Some((a.clone(), run_id.clone()));
        }
        self.settle_notice();
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: true,
            at: Instant::now(),
            about: None,
            seen: None,
        });
    }

    /// Clears a run notice whose run changed its lifecycle stage or ended.
    fn settle_notice(&mut self) {
        let Some(n) = &mut self.notice else { return };
        let Some((a, id)) = &n.about else { return };
        let now = self
            .active
            .get(a)
            .filter(|r| &r.run_id == id)
            .map(|r| stage(r.lifecycle));
        let ended = now.is_none()
            && (n.seen.is_some() || self.last.get(a).is_some_and(|l| &l.run_id == id));
        let moved = now.is_some() && n.seen.is_some() && n.seen != now;
        if ended || moved {
            self.notice = None;
        } else if now.is_some() {
            n.seen = now;
        }
    }

    fn error_info(&mut self, what: &str, e: &ErrorInfo) {
        let mut s = format!("{what}: [{}] {}", e.code, e.message);
        if let Some(n) = &e.next_action {
            s.push_str(&format!("  (try: {})", n.argv.join(" ")));
        }
        self.error(s);
    }

    // ----- selection ----------------------------------------------------------------

    pub fn item(&self, a: &ActionRef) -> Option<&Item> {
        self.items.iter().find(|i| &i.action_ref == a)
    }

    pub fn selected_entry(&self) -> Option<Entry> {
        self.visible.get(self.selected).copied()
    }

    pub fn selected_item(&self) -> Option<&Item> {
        match self.selected_entry()? {
            Entry::Action(i) => self.items.get(i),
            Entry::View(_) => None,
        }
    }

    pub fn selected_view(&self) -> Option<&ViewItem> {
        match self.selected_entry()? {
            Entry::View(i) => self.views.get(i),
            Entry::Action(_) => None,
        }
    }

    pub fn selected_ref(&self) -> Option<ActionRef> {
        self.selected_item().map(|i| i.action_ref.clone())
    }

    pub fn selected_view_pane(&self) -> Option<&ViewPane> {
        self.selected_view()
            .and_then(|v| self.view_panes.get(&v.view_ref))
    }

    fn entry_key(&self, e: Entry) -> Option<ItemRef> {
        match e {
            Entry::Action(i) => self.items.get(i).map(|x| x.action_ref.to_item_ref()),
            Entry::View(i) => self.views.get(i).map(|x| x.view_ref.to_item_ref()),
        }
    }

    fn selected_key(&self) -> Option<ItemRef> {
        self.selected_entry().and_then(|e| self.entry_key(e))
    }

    /// Lists the entries that match the filter, best match first (see [`rank`]); without a
    /// filter, every entry in plugin order. Matches stay grouped under their plugin: plugins
    /// are ordered by their best match, entries within a plugin by their own rank, so each
    /// plugin heading appears once. `keep` stays selected if it still matches.
    fn refilter(&mut self, keep: Option<ItemRef>) {
        let words: Vec<String> = self
            .filter
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut out: Vec<(usize, (u8, std::cmp::Reverse<usize>), Entry)> = Vec::new();
        for (pi, p) in self.plugin_order.iter().enumerate() {
            for (n, i) in self.items.iter().enumerate() {
                if i.action_ref.plugin.as_str() != p {
                    continue;
                }
                let r = i.action_ref.to_string();
                let id = i.action_ref.action.to_string();
                if let Some(k) = rank(&words, &r, &id, &i.title, &i.tags, &[], &i.description) {
                    out.push((pi, k, Entry::Action(n)));
                }
            }
            for (n, v) in self.views.iter().enumerate() {
                if v.view_ref.plugin.as_str() != p {
                    continue;
                }
                let r = v.view_ref.to_string();
                let id = v.view_ref.view.to_string();
                let kind = [crate::views::kind_word(v.kind).to_owned()];
                if let Some(k) = rank(&words, &r, &id, &v.title, &v.tags, &kind, &v.description) {
                    out.push((pi, k, Entry::View(n)));
                }
            }
        }
        let mut best: HashMap<usize, (u8, std::cmp::Reverse<usize>)> = HashMap::new();
        for (pi, k, _) in &out {
            best.entry(*pi)
                .and_modify(|b| *b = (*b).min(*k))
                .or_insert(*k);
        }
        // A stable sort keeps the list order among equal matches.
        out.sort_by_key(|(pi, k, _)| (best.get(pi).copied(), *pi, *k));
        self.visible = out.into_iter().map(|(_, _, e)| e).collect();
        self.selected = keep
            .and_then(|k| {
                self.visible
                    .iter()
                    .position(|&e| self.entry_key(e).as_ref() == Some(&k))
            })
            .unwrap_or(0)
            .min(self.visible.len().saturating_sub(1));
    }

    fn on_select(&mut self) {
        self.output_top = 0;
        let sel = self.selected_ref();
        if self
            .notice
            .as_ref()
            .and_then(|n| n.about.as_ref())
            .is_some_and(|(a, _)| Some(a) != sel.as_ref())
        {
            self.notice = None;
        }
        if let Some(v) = self.selected_view() {
            let r = v.view_ref.clone();
            if !self.view_panes.contains_key(&r) {
                self.load_view(&r);
                let _ = self.io.read.send(Read::DescribeView(r));
            }
            return;
        }
        let Some(item) = self.selected_item() else {
            return;
        };
        let a = item.action_ref.clone();
        let hash = item.definition_hash.clone();
        if self.inputs.get(&a).is_none_or(|(h, _)| *h != hash) {
            self.inputs.insert(a.clone(), (hash, Inputs::Loading));
            let _ = self.io.read.send(Read::Describe(a.clone()));
        }
        self.sync_pane(&a);
    }

    /// Makes the action's panel show the run chosen in the history list, else its current
    /// (or latest) run.
    fn sync_pane(&mut self, a: &ActionRef) {
        let want = self
            .viewing
            .get(a)
            .map(|r| r.run_id.clone())
            .or_else(|| self.active.get(a).map(|r| r.run_id.clone()))
            .or_else(|| self.last.get(a).map(|l| l.run_id.clone()));
        self.pane_order.retain(|x| x != a);
        self.pane_order.push_back(a.clone());
        while self.pane_order.len() > MAX_PANES {
            if let Some(old) = self.pane_order.pop_front() {
                self.panes.remove(&old);
            }
        }
        match self.panes.get_mut(a) {
            None => {
                let mut p = LogPane::new(a.clone());
                p.run_id = want.clone();
                p.loading = true;
                self.panes.insert(a.clone(), p);
                let _ = self.io.read.send(Read::Tail(a.clone(), want));
            }
            Some(p) => {
                if let Some(w) = want
                    && p.run_id.as_ref() != Some(&w)
                {
                    p.reset(Some(w.clone()));
                    p.loading = true;
                    let _ = self.io.read.send(Read::Tail(a.clone(), Some(w)));
                }
            }
        }
    }

    pub fn selected_pane(&self) -> Option<&LogPane> {
        self.selected_ref().and_then(|a| self.panes.get(&a))
    }

    pub fn selected_pane_mut(&mut self) -> Option<&mut LogPane> {
        let a = self.selected_ref()?;
        self.panes.get_mut(&a)
    }

    /// The tab the main pane shows: `History` while the history list of the selected action
    /// is open, else the chosen tab.
    pub fn shown_tab(&self) -> Tab {
        match &self.modal {
            Modal::History { action_ref, .. }
                if Some(action_ref) == self.selected_ref().as_ref() =>
            {
                Tab::History
            }
            _ => self.tab,
        }
    }

    /// Reads the selected action's newest runs for the right rail when its run state changed
    /// since the last read. Cheap to call on every frame.
    pub fn want_recent(&mut self) {
        let Some(a) = self.selected_ref() else {
            return;
        };
        let key = match (self.active.get(&a), self.last.get(&a)) {
            (Some(r), _) => (Some(r.run_id.clone()), stage(r.lifecycle), false),
            (None, Some(l)) => (Some(l.run_id.clone()), 2, l.ended_at.is_some()),
            (None, None) => (None, 0, false),
        };
        if key.0.is_none() || self.recent_key.get(&a) == Some(&key) {
            return;
        }
        self.recent_key.insert(a.clone(), key);
        let _ = self.io.read.send(Read::History(a));
    }

    /// Moves to `tab`; `History` opens the history list of the selected action.
    fn go_tab(&mut self, tab: Tab) {
        if matches!(self.modal, Modal::History { .. }) {
            self.modal = Modal::None;
        }
        match tab {
            Tab::History => self.exec(Cmd::History),
            t => self.tab = t,
        }
    }

    // ----- item actions -------------------------------------------------------------

    fn schema(&self, a: &ActionRef) -> Option<&JsonObject> {
        match self.inputs.get(a) {
            Some((_, Inputs::Form(s))) => Some(s),
            _ => None,
        }
    }

    fn required(&self, a: &ActionRef) -> bool {
        self.schema(a)
            .is_some_and(|s| !form::required_names(s).is_empty())
    }

    fn can_act(&self, item: &Item) -> bool {
        item.enabled && self.control_lost.is_none() && !self.pending.contains_key(&item.action_ref)
    }

    fn open_kind(&self, item: &Item) -> Option<OpenKind> {
        if self.active.contains_key(&item.action_ref) || self.pending.contains_key(&item.action_ref)
        {
            return Some(OpenKind::Logs);
        }
        if !self.can_act(item) {
            return None;
        }
        if self.required(&item.action_ref) {
            Some(OpenKind::NeedsInput)
        } else if self.schema(&item.action_ref).is_some() {
            Some(OpenKind::Form)
        } else {
            Some(OpenKind::Start)
        }
    }

    /// The `s` action: start/run when idle, stop when active; `None` while stopping.
    fn toggle_intent(&self, item: &Item) -> Option<Intent> {
        if !self.can_act(item) {
            return None;
        }
        match self.active.get(&item.action_ref) {
            Some(r) if matches!(r.lifecycle, Lifecycle::Stopping { .. }) => None,
            Some(_) => Some(Intent::Stop),
            None => Some(Intent::Start),
        }
    }

    fn restart_ok(&self, item: &Item) -> bool {
        if !self.can_act(item) {
            return false;
        }
        match self.active.get(&item.action_ref) {
            Some(r) => !matches!(r.lifecycle, Lifecycle::Stopping { .. }),
            None => item.mode == ActionMode::Task && self.last.contains_key(&item.action_ref),
        }
    }

    fn intent(&mut self, a: &ActionRef, intent: Intent) {
        if intent != Intent::Stop {
            match self.inputs.get(a) {
                None | Some((_, Inputs::Loading)) => {
                    self.queued = Some((a.clone(), intent));
                    if !self.inputs.contains_key(a) {
                        let hash = self.item(a).map(|i| i.definition_hash.clone());
                        if let Some(h) = hash {
                            self.inputs.insert(a.clone(), (h, Inputs::Loading));
                        }
                        let _ = self.io.read.send(Read::Describe(a.clone()));
                    }
                    self.info(format!("reading {a} inputs..."));
                    return;
                }
                Some((_, Inputs::Form(schema))) => {
                    let form = Form::new(a.clone(), intent, schema, self.last_inputs.get(a));
                    self.modal = Modal::Form(Box::new(form));
                    return;
                }
                Some((_, Inputs::Free | Inputs::Unknown(_))) => {}
            }
        }
        self.act(a, intent, JsonObject::new(), false);
    }

    /// `from_form`: the input came from a submitted form, which waits for the answer.
    fn act(&mut self, a: &ActionRef, intent: Intent, input: JsonObject, from_form: bool) {
        let run = self.active.get(a).map(|r| r.run_id.clone());
        match (intent, run) {
            (Intent::Stop, Some(run_id)) => self.stop(a, run_id),
            (Intent::Restart, Some(run_id)) => {
                self.restart_after
                    .insert(a.clone(), (run_id.clone(), input));
                self.stop(a, run_id);
            }
            (Intent::Stop, None) => {}
            (Intent::Start, Some(_)) if !from_form => self.focus = Focus::Logs,
            (Intent::Start | Intent::Restart, _) => self.invoke(a.clone(), input),
        }
    }

    fn stop(&mut self, a: &ActionRef, run_id: RunId) {
        self.pending.insert(a.clone(), Intent::Stop);
        let _ = self.io.control.send(Control::Stop {
            action_ref: a.clone(),
            run_id,
        });
    }

    fn invoke(&mut self, a: ActionRef, input: JsonObject) {
        self.pending.insert(a.clone(), Intent::Start);
        let _ = self.io.control.send(Control::Invoke {
            action_ref: a,
            input,
        });
    }

    fn run_row_action(&mut self, view_ref: ViewRef, action: ActionId) {
        let Some(p) = self.view_panes.get(&view_ref) else {
            return;
        };
        let (Some(row), Some(expected)) = (p.selected_row_id(), p.sel_rev) else {
            self.error("select a row first");
            return;
        };
        self.info(format!(
            "running {}.{action} for row {row} of revision {expected}...",
            view_ref.plugin
        ));
        let _ = self.io.control.send(Control::ViewAction {
            view_ref,
            action,
            row,
            expected,
        });
    }

    // ----- keys ---------------------------------------------------------------------

    /// Every key that works now. The footer shows the `footer` ones; the router accepts
    /// only these.
    pub fn bindings(&self) -> Vec<Binding> {
        if let Some(v) = self.term.bindings() {
            return v;
        }
        let mut v = Vec::new();
        match &self.modal {
            Modal::Search { logs, .. } => {
                v.push(bind(
                    "type",
                    if *logs { "find text" } else { "filter" },
                    Cmd::Search,
                ));
                v.push(bind("Enter", "apply", Cmd::Open));
                v.push(bind("Esc", "cancel", Cmd::Escape));
                return v;
            }
            Modal::Form(f) => {
                v.push(bind("Tab/Up/Down", "field", Cmd::Down));
                if f.focused()
                    .is_some_and(|x| matches!(x.kind, form::Kind::Boolean | form::Kind::Enum(_)))
                {
                    v.push(bind("Space", "choose", Cmd::Toggle));
                } else {
                    v.push(bind("Ctrl-U", "clear", Cmd::Escape));
                }
                if !f.pending {
                    // Only a process restarts; a task runs again.
                    let process = self
                        .item(&f.action_ref)
                        .is_some_and(|i| i.mode == ActionMode::Process);
                    let w = match f.intent {
                        Intent::Restart if process => "restart",
                        _ => "run",
                    };
                    v.push(bind("Enter", w, Cmd::Open));
                }
                v.push(bind("Esc", "cancel", Cmd::Escape));
                return v;
            }
            Modal::Command { .. } => {
                v.push(bind("Enter", "run mira command", Cmd::Open));
                v.push(bind("Esc", "cancel", Cmd::Escape));
                return v;
            }
            Modal::Output(_) => {
                v.push(bind("j/k", "scroll", Cmd::Down));
                v.push(bind("y", "copy all", Cmd::Copy));
                v.push(bind("Esc/q", "close", Cmd::Escape));
                return v;
            }
            Modal::RowAction { .. } => {
                v.push(bind("j/k", "choose", Cmd::Down));
                v.push(bind("Enter", "run for this row", Cmd::Open));
                v.push(bind("Esc", "cancel", Cmd::Escape));
                return v;
            }
            Modal::History { runs, .. } => {
                if runs.as_ref().is_some_and(|r| !r.is_empty()) {
                    v.push(bind("j/k", "choose", Cmd::Down));
                    v.push(bind("Enter", "open logs", Cmd::Open));
                }
                v.push(bind("[ ]", "tab", Cmd::NextTab));
                v.push(bind("Esc", "close", Cmd::Escape));
                return v;
            }
            Modal::Help { max, .. } => {
                if *max > 0 {
                    v.push(bind("j/k", "scroll", Cmd::Down));
                }
                v.push(bind("Esc/?", "close help", Cmd::Escape));
                return v;
            }
            Modal::None => {}
        }
        self.normal_bindings()
    }

    /// The bindings of normal mode for the current focus and selection.
    pub fn normal_bindings(&self) -> Vec<Binding> {
        if self.selected_view().is_some() {
            return self.view_bindings();
        }
        let mut v = Vec::new();
        // The log keys act on the Logs tab only.
        let output = self.tab == Tab::Output;
        let pane = self
            .selected_pane()
            .filter(|p| !p.records.is_empty() && !output);
        match self.focus {
            Focus::List => {
                if self.visible.len() > 1 {
                    v.push(bind("j/k", "move", Cmd::Down));
                    v.push(hidden("PgUp/PgDn", "page", Cmd::PageUp));
                    v.push(hidden("g/Home", "first", Cmd::Top));
                    v.push(hidden("G/End", "last", Cmd::Bottom));
                }
            }
            Focus::Logs => {
                if output {
                    v.push(bind("j/k", "scroll", Cmd::Down));
                }
                if pane.is_some() {
                    v.push(bind("j/k", "scroll", Cmd::Down));
                    v.push(hidden("PgUp/PgDn", "page", Cmd::PageUp));
                    v.push(hidden("g/Home", "oldest", Cmd::Top));
                    let follow = if pane.is_some_and(LogPane::is_pinned) {
                        "follow"
                    } else {
                        "bottom"
                    };
                    v.push(bind("G/End", follow, Cmd::Bottom));
                }
                if pane.is_none_or(|p| p.anchor.is_none())
                    && self
                        .selected_ref()
                        .is_some_and(|a| self.viewing.contains_key(&a))
                {
                    v.push(bind("Esc", "latest run", Cmd::Escape));
                }
            }
        }
        if let Some(item) = self.selected_item() {
            match self.open_kind(item) {
                Some(OpenKind::Start) => v.push(bind("Enter", start_word(item), Cmd::Open)),
                Some(OpenKind::Logs) if self.focus == Focus::List => {
                    v.push(bind("Enter", "logs", Cmd::Open))
                }
                Some(OpenKind::NeedsInput) => v.push(bind("Enter", "form", Cmd::Open)),
                Some(OpenKind::Form) => {
                    v.push(bind("Enter", format!("{}...", start_word(item)), Cmd::Open))
                }
                _ => {}
            }
            if self.attach_target().is_some() {
                v.push(bind("a", "attach terminal", Cmd::Attach));
            }
            match self.toggle_intent(item) {
                Some(Intent::Stop) => v.push(bind("s", "stop", Cmd::Toggle)),
                // When Enter already shows the same word, keep `s` in help only.
                Some(_)
                    if v.iter()
                        .any(|b| b.keys == "Enter" && b.label == start_word(item)) =>
                {
                    v.push(hidden("s", start_word(item), Cmd::Toggle))
                }
                Some(_) => v.push(bind("s", start_word(item), Cmd::Toggle)),
                None => {}
            }
            if self.restart_ok(item) {
                let w = if item.mode == ActionMode::Process {
                    "restart"
                } else {
                    "rerun"
                };
                v.push(bind("r", w, Cmd::Restart));
            }
            if self.has_history(&item.action_ref) {
                v.push(bind("H", "history", Cmd::History));
            }
            if self.has_schedule(&item.action_ref)
                && self.control_lost.is_none()
                && !self.pending.contains_key(&item.action_ref)
            {
                let on = self
                    .schedule_of(&item.action_ref)
                    .is_some_and(|s| s.enabled);
                let w = if on { "schedule off" } else { "schedule on" };
                v.push(bind("t", w, Cmd::Schedule));
            }
        }
        if self.selected_item().is_some() {
            v.push(hidden("[ ]", "previous or next tab", Cmd::NextTab));
            v.push(hidden(
                "1 2 3",
                "Logs, History, or Output tab",
                Cmd::GoTab(0),
            ));
            let to = if self.focus == Focus::List {
                "logs"
            } else {
                "tools"
            };
            v.push(bind("Tab", to, Cmd::Focus));
        }
        if self.focus == Focus::Logs
            && let Some(p) = pane
        {
            let n = p.selection_text().map_or(1, |(_, n)| n);
            let what = if n == 1 {
                "copy".to_owned()
            } else {
                format!("copy {n} lines")
            };
            v.push(bind("y", what, Cmd::Copy));
            if p.anchor.is_none() {
                v.push(bind("v", "select", Cmd::Select));
            } else {
                v.push(bind("Esc", "unselect", Cmd::Escape));
            }
            v.push(bind("/", "find", Cmd::Search));
            if self.log_query.is_some() {
                v.push(bind("n/N", "next/prev", Cmd::NextMatch));
            }
            if !p.wrap {
                v.push(bind("h/l", "pan", Cmd::Right));
            }
            v.push(bind(
                "w",
                if p.wrap { "no wrap" } else { "wrap" },
                Cmd::Wrap,
            ));
        }
        if self.focus == Focus::List {
            if !self.items.is_empty() {
                v.push(bind("/", "search", Cmd::Search));
            }
            if !self.filter.is_empty() {
                v.push(bind("Esc", "clear filter", Cmd::Escape));
            }
        }
        self.global_bindings(&mut v);
        v
    }

    fn global_bindings(&self, v: &mut Vec<Binding>) {
        if self.focus == Focus::List && !self.filter.is_empty() && self.selected_view().is_some() {
            v.push(bind("Esc", "clear filter", Cmd::Escape));
        }
        v.push(bind(":", "command", Cmd::Command));
        v.push(hidden(
            "m",
            if self.mouse {
                "mouse mode off (terminal selection)"
            } else {
                "mouse mode on (wheel scrolls)"
            },
            Cmd::Mouse,
        ));
        if self.control_lost.is_none()
            && !self.keeping
            && self.is_controller()
            && self
                .session
                .as_ref()
                .is_some_and(|s| s.state == SessionState::Active)
        {
            v.push(bind("b", "background", Cmd::Keep));
        }
        v.push(bind("?", "help", Cmd::Help));
        v.push(bind(
            "q",
            format!("quit ({})", self.quit_effect(true)),
            Cmd::Quit,
        ));
    }

    /// Keys for a selected view (list or view focus).
    fn view_bindings(&self) -> Vec<Binding> {
        let mut v = Vec::new();
        let pane = self.selected_view_pane();
        let rows = pane.map_or(0, ViewPane::len);
        match self.focus {
            Focus::List => {
                if self.visible.len() > 1 {
                    v.push(bind("j/k", "move", Cmd::Down));
                    v.push(hidden("PgUp/PgDn", "page", Cmd::PageUp));
                    v.push(hidden("g/Home", "first", Cmd::Top));
                    v.push(hidden("G/End", "last", Cmd::Bottom));
                }
                v.push(bind("Enter", "open view", Cmd::Open));
                v.push(bind("Tab", "view", Cmd::Focus));
                v.push(bind("r", "refresh", Cmd::Restart));
                if !self.items.is_empty() || !self.views.is_empty() {
                    v.push(bind("/", "search", Cmd::Search));
                }
            }
            Focus::Logs => {
                // First, so the footer keeps it at narrow widths.
                v.push(bind("Esc", "back", Cmd::Escape));
                if rows > 0 {
                    v.push(bind("j/k", "move", Cmd::Down));
                    v.push(hidden("PgUp/PgDn", "page", Cmd::PageUp));
                    v.push(hidden("g/Home", "first", Cmd::Top));
                    v.push(hidden("G/End", "last", Cmd::Bottom));
                }
                if let Some(p) = pane {
                    if p.kind == ViewKind::Table
                        && rows > 0
                        && !p.row_actions.is_empty()
                        && self.control_lost.is_none()
                    {
                        let w = if p.row_actions.len() == 1 {
                            format!("run {}", p.row_actions[0])
                        } else {
                            "row actions".into()
                        };
                        v.push(bind("Enter", w, Cmd::Open));
                    }
                    if rows > 0 {
                        let (y, big) = match p.kind {
                            ViewKind::Table => ("copy cell", "copy row JSON"),
                            ViewKind::Log => ("copy text", "copy item JSON"),
                            ViewKind::Tree => ("copy label", "copy subtree JSON"),
                            ViewKind::Json => ("copy line", "copy all JSON"),
                            ViewKind::Text => ("copy line", "copy all text"),
                        };
                        v.push(bind("y", y, Cmd::Copy));
                        v.push(bind("Y", big, Cmd::CopyAll));
                        let lr = match p.kind {
                            ViewKind::Table => "column",
                            ViewKind::Tree => "fold",
                            _ => "pan",
                        };
                        if p.kind != ViewKind::Table || p.data().is_some() {
                            v.push(bind("h/l", lr, Cmd::Right));
                        }
                    }
                    if p.can_wrap() {
                        v.push(bind(
                            "w",
                            if p.wrap { "no wrap" } else { "wrap" },
                            Cmd::Wrap,
                        ));
                    }
                }
                v.push(bind("r", "refresh", Cmd::Restart));
                v.push(bind("Tab", "tools", Cmd::Focus));
            }
        }
        self.global_bindings(&mut v);
        v
    }

    /// The action has at least one run the history list can show.
    fn has_history(&self, a: &ActionRef) -> bool {
        self.active.contains_key(a) || self.last.contains_key(a) || self.viewing.contains_key(a)
    }

    /// Shows `rec`'s logs in the action's pane. The newest run clears the historical choice,
    /// so the pane follows new runs again.
    fn open_history_run(&mut self, a: ActionRef, rec: RunRecord) {
        let latest = self
            .active
            .get(&a)
            .map(|r| &r.run_id)
            .or_else(|| self.last.get(&a).map(|l| &l.run_id));
        if latest == Some(&rec.run_id) {
            self.viewing.remove(&a);
        } else {
            self.viewing.insert(a.clone(), rec);
        }
        self.sync_pane(&a);
        self.focus = Focus::Logs;
        self.tab = Tab::Logs;
    }

    /// The selected item's active PTY run, if it can be attached.
    fn attach_target(&self) -> Option<(ActionRef, RunId)> {
        let item = self.selected_item()?;
        let run = self.active.get(&item.action_ref)?;
        (self.term.is_pty(&item.action_ref) && run.lifecycle.is_active())
            .then(|| (item.action_ref.clone(), run.run_id.clone()))
    }

    pub fn is_controller(&self) -> bool {
        self.control_lost.is_none()
            && self
                .session
                .as_ref()
                .is_some_and(|s| Some(&s.id) == self.attached_to.as_ref())
    }

    /// What closing this TUI does to the session's work (§5.1).
    pub fn quit_effect(&self, short: bool) -> String {
        let runs = self.active.len() + self.adhoc_runs;
        let Some(s) = &self.session else {
            return if short {
                "no session".into()
            } else {
                "No work session was running.".into()
            };
        };
        if !self.is_controller() {
            return if short {
                "work continues".into()
            } else {
                "This TUI was not a controller of the current session; its work continues.".into()
            };
        }
        if s.state == SessionState::Stopping {
            return if short {
                "session stopping".into()
            } else {
                "The session was already stopping.".into()
            };
        }
        if s.controller_count > 1 {
            let others = s.controller_count - 1;
            return if short {
                "work continues".into()
            } else {
                format!(
                    "Work continues: {others} other controller(s) still attached ({runs} active run(s))."
                )
            };
        }
        if s.background_lease && s.expires_at.is_none() {
            return if short {
                "kept in background".into()
            } else {
                format!(
                    "Work continues under a background lease without expiry ({runs} active run(s)); `mira down` stops it."
                )
            };
        }
        if let Some(t) = s.expires_at {
            return if short {
                format!("kept until {}", self.clock(t, false))
            } else {
                format!(
                    "Work continues in the background until {} ({runs} active run(s)); `mira down` stops it.",
                    self.clock(t, false)
                )
            };
        }
        if runs == 0 {
            if short {
                "ends session".into()
            } else {
                "This was the last controller; the session ends.".into()
            }
        } else if short {
            format!("stops {runs} run{}", if runs == 1 { "" } else { "s" })
        } else {
            format!(
                "This was the last controller: the host is stopping {runs} run(s) this session owned."
            )
        }
    }

    pub fn clock(&self, t: Timestamp, seconds: bool) -> String {
        let nanos = i128::from(t.unix_ms()) * 1_000_000;
        match time::OffsetDateTime::from_unix_timestamp_nanos(nanos) {
            Ok(dt) => {
                let dt = dt.to_offset(self.offset);
                if seconds {
                    format!("{:02}:{:02}:{:02}", dt.hour(), dt.minute(), dt.second())
                } else {
                    format!("{:02}:{:02}", dt.hour(), dt.minute())
                }
            }
            Err(_) => "--:--".into(),
        }
    }

    pub fn key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        if self.term.is_open() {
            return self.term.key(k);
        }
        let repeat = k.kind == KeyEventKind::Repeat;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match &self.modal {
            Modal::Search { logs, .. } => {
                let logs = *logs;
                match k.code {
                    KeyCode::Esc => self.cancel_search(),
                    KeyCode::Char('c') if ctrl => self.cancel_search(),
                    KeyCode::Enter => self.apply_search(),
                    KeyCode::Backspace | KeyCode::Char(_) => {
                        let Modal::Search { text, .. } = &mut self.modal else {
                            return;
                        };
                        match k.code {
                            KeyCode::Backspace => {
                                text.pop();
                            }
                            KeyCode::Char(c) if !ctrl => text.push(c),
                            _ => return,
                        }
                        let text = text.clone();
                        if !logs {
                            // The best match is selected as the filter changes.
                            self.filter = text;
                            self.refilter(None);
                            self.on_select();
                        }
                    }
                    _ => {}
                }
                return;
            }
            Modal::Form(_) => {
                if repeat && k.code == KeyCode::Enter {
                    return;
                }
                let Modal::Form(f) = &mut self.modal else {
                    return;
                };
                match f.key(k) {
                    Outcome::Stay => {}
                    Outcome::Cancel => self.modal = Modal::None,
                    Outcome::Submit(input) => {
                        let a = f.action_ref.clone();
                        let intent = f.intent;
                        let keep = Form::remembered(&input, &f.fields);
                        self.last_inputs.insert(a.clone(), keep);
                        self.act(&a, intent, input, true);
                    }
                }
                return;
            }
            Modal::Command { .. } => {
                let Modal::Command { text, error } = &mut self.modal else {
                    return;
                };
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Char('c') if ctrl => self.modal = Modal::None,
                    KeyCode::Char('u') if ctrl => text.clear(),
                    KeyCode::Backspace => {
                        text.pop();
                        *error = None;
                    }
                    KeyCode::Enter if !repeat => match cmdbar::split(text) {
                        Ok(words) => {
                            let t = format!("mira {}", words.join(" "));
                            *error = Some(format!("running {t}..."));
                            cmdbar::run(self.root.clone(), words, self.io.events.clone());
                        }
                        Err(e) => *error = Some(e),
                    },
                    KeyCode::Char(c) if !ctrl => {
                        text.push(c);
                        *error = None;
                    }
                    _ => {}
                }
                return;
            }
            Modal::Output(_) => {
                let Modal::Output(o) = &mut self.modal else {
                    return;
                };
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.modal = Modal::None,
                    KeyCode::Char('c') if ctrl => self.modal = Modal::None,
                    KeyCode::Char('j') | KeyCode::Down => {
                        o.top = (o.top + 1).min(o.lines.len().saturating_sub(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => o.top = o.top.saturating_sub(1),
                    KeyCode::PageDown => o.top = (o.top + 10).min(o.lines.len().saturating_sub(1)),
                    KeyCode::PageUp => o.top = o.top.saturating_sub(10),
                    KeyCode::Char('y') => {
                        let n = o.lines.len();
                        clip::copy(o.text.clone(), n, self.io.events.clone());
                    }
                    _ => {}
                }
                return;
            }
            Modal::RowAction { .. } => {
                let Modal::RowAction { choices, index, .. } = &mut self.modal else {
                    return;
                };
                match k.code {
                    KeyCode::Esc => self.modal = Modal::None,
                    KeyCode::Char('c') if ctrl => self.modal = Modal::None,
                    KeyCode::Char('j') | KeyCode::Down => {
                        *index = (*index + 1).min(choices.len().saturating_sub(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => *index = index.saturating_sub(1),
                    KeyCode::Enter if !repeat => {
                        if let Modal::RowAction {
                            view_ref,
                            choices,
                            index,
                        } = std::mem::replace(&mut self.modal, Modal::None)
                            && let Some(a) = choices.get(index)
                        {
                            self.run_row_action(view_ref, a.clone());
                        }
                    }
                    _ => {}
                }
                return;
            }
            Modal::History { .. } => {
                let Modal::History { runs, index, .. } = &mut self.modal else {
                    return;
                };
                let n = runs.as_ref().map_or(0, Vec::len);
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('H') => {
                        self.modal = Modal::None
                    }
                    KeyCode::Char('[') | KeyCode::Char('1') => self.go_tab(Tab::Logs),
                    KeyCode::Char(']') | KeyCode::Char('3') => self.go_tab(Tab::Output),
                    KeyCode::Tab | KeyCode::BackTab => {
                        self.modal = Modal::None;
                        self.exec(Cmd::Focus);
                    }
                    KeyCode::Char('c') if ctrl => self.modal = Modal::None,
                    KeyCode::Char('j') | KeyCode::Down => {
                        *index = (*index + 1).min(n.saturating_sub(1))
                    }
                    KeyCode::Char('k') | KeyCode::Up => *index = index.saturating_sub(1),
                    KeyCode::Enter if !repeat && n > 0 => {
                        if let Modal::History {
                            action_ref,
                            runs: Some(runs),
                            index,
                            ..
                        } = std::mem::replace(&mut self.modal, Modal::None)
                            && let Some(rec) = runs.into_iter().nth(index)
                        {
                            self.open_history_run(action_ref, rec);
                        }
                    }
                    _ => {}
                }
                return;
            }
            Modal::Help { .. } => {
                let Modal::Help { top, max } = &mut self.modal else {
                    return;
                };
                match k.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
                        self.modal = Modal::None
                    }
                    KeyCode::Char('c') if ctrl => self.modal = Modal::None,
                    KeyCode::Char('j') | KeyCode::Down => *top = (*top + 1).min(*max),
                    KeyCode::Char('k') | KeyCode::Up => *top = top.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => *top = (*top + 10).min(*max),
                    KeyCode::PageUp => *top = top.saturating_sub(10),
                    KeyCode::Char('g') | KeyCode::Home => *top = 0,
                    KeyCode::Char('G') | KeyCode::End => *top = *max,
                    _ => {}
                }
                return;
            }
            Modal::None => {}
        }
        let cmd = match (k.code, ctrl) {
            (KeyCode::Char('c'), true) => Cmd::Quit,
            (KeyCode::Char('d'), true) => Cmd::PageDown,
            (KeyCode::Char('u'), true) => Cmd::PageUp,
            (_, true) => return,
            (KeyCode::Char('j') | KeyCode::Down, _) => Cmd::Down,
            (KeyCode::Char('k') | KeyCode::Up, _) => Cmd::Up,
            (KeyCode::PageDown, _) => Cmd::PageDown,
            (KeyCode::PageUp, _) => Cmd::PageUp,
            (KeyCode::Char('g') | KeyCode::Home, _) => Cmd::Top,
            (KeyCode::Char('G') | KeyCode::End, _) => Cmd::Bottom,
            (KeyCode::Enter, _) => Cmd::Open,
            (KeyCode::Char('s'), _) => Cmd::Toggle,
            (KeyCode::Char('r'), _) => Cmd::Restart,
            (KeyCode::Tab | KeyCode::BackTab, _) => Cmd::Focus,
            (KeyCode::Char('/'), _) => Cmd::Search,
            (KeyCode::Char('q'), _) => Cmd::Quit,
            (KeyCode::Char('b'), _) => Cmd::Keep,
            (KeyCode::Char('h') | KeyCode::Left, _) => Cmd::Left,
            (KeyCode::Char('l') | KeyCode::Right, _) => Cmd::Right,
            (KeyCode::Char('w'), _) => Cmd::Wrap,
            (KeyCode::Char('y'), _) => Cmd::Copy,
            (KeyCode::Char('v'), _) => Cmd::Select,
            (KeyCode::Esc, _) => Cmd::Escape,
            (KeyCode::Char('n'), _) => Cmd::NextMatch,
            (KeyCode::Char('N'), _) => Cmd::PrevMatch,
            (KeyCode::Char('?'), _) => Cmd::Help,
            (KeyCode::Char('a'), _) => Cmd::Attach,
            (KeyCode::Char(':'), _) => Cmd::Command,
            (KeyCode::Char('t'), _) => Cmd::Schedule,
            (KeyCode::Char('m'), _) => Cmd::Mouse,
            (KeyCode::Char('Y'), _) => Cmd::CopyAll,
            (KeyCode::Char('H'), _) => Cmd::History,
            (KeyCode::Char(']'), _) => Cmd::NextTab,
            (KeyCode::Char('['), _) => Cmd::PrevTab,
            (KeyCode::Char(c @ '1'..='3'), _) => Cmd::GoTab(c as u8 - b'1'),
            _ => return,
        };
        let navigation = matches!(
            cmd,
            Cmd::Up | Cmd::Down | Cmd::PageUp | Cmd::PageDown | Cmd::Left | Cmd::Right
        );
        // Auto-repeat moves; it never repeats stop, restart, or other effects.
        if repeat && !navigation {
            return;
        }
        // One binding covers both directions of a pair (j/k, PgUp/PgDn, h/l, n/N).
        let bound = self.bindings().iter().any(|b| {
            b.cmd == cmd
                || (cmd == Cmd::Up && b.cmd == Cmd::Down)
                || (cmd == Cmd::PageDown && b.cmd == Cmd::PageUp)
                || (cmd == Cmd::Left && b.cmd == Cmd::Right)
                || (cmd == Cmd::PrevMatch && b.cmd == Cmd::NextMatch)
                || (cmd == Cmd::PrevTab && b.cmd == Cmd::NextTab)
                || matches!((cmd, b.cmd), (Cmd::GoTab(_), Cmd::GoTab(_)))
        });
        if bound {
            self.exec(cmd);
        }
    }

    fn cancel_search(&mut self) {
        if let Modal::Search {
            logs, prev_filter, ..
        } = std::mem::replace(&mut self.modal, Modal::None)
            && !logs
        {
            self.filter = prev_filter;
            let keep = self.selected_key();
            self.refilter(keep);
            self.on_select();
        }
    }

    fn apply_search(&mut self) {
        let Modal::Search { logs, text, .. } = std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };
        if !logs {
            // Enter puts the cursor on the best match.
            self.selected = 0;
            self.on_select();
            return;
        }
        if text.is_empty() {
            self.log_query = None;
            return;
        }
        self.log_query = Some(text.clone());
        self.find(&text, true);
    }

    fn find(&mut self, q: &str, older: bool) {
        let found = self.selected_pane_mut().is_some_and(|p| p.find(q, older));
        if !found {
            let dir = if older { "above" } else { "below" };
            self.info(format!(
                "no match for \"{q}\" {dir} the cursor in the loaded lines"
            ));
        }
    }

    fn exec(&mut self, cmd: Cmd) {
        if self.selected_view().is_some() && self.exec_view(cmd) {
            return;
        }
        match cmd {
            Cmd::Command => {
                self.modal = Modal::Command {
                    text: String::new(),
                    error: None,
                }
            }
            Cmd::Mouse => {
                self.mouse = !self.mouse;
                self.mouse_changed = Some(self.mouse);
                self.info(if self.mouse {
                    "mouse mode on: the wheel scrolls; hold Option (or Shift) for terminal selection"
                } else {
                    "mouse mode off: the terminal's own selection works"
                });
            }
            Cmd::Schedule => {
                if let Some(a) = self.selected_ref()
                    && self.has_schedule(&a)
                {
                    let enabled = !self.schedule_of(&a).is_some_and(|s| s.enabled);
                    self.pending.insert(a.clone(), Intent::Start);
                    let _ = self.io.control.send(Control::Schedule {
                        action_ref: a,
                        enabled,
                    });
                }
            }
            Cmd::CopyAll => {}
            Cmd::NextTab | Cmd::PrevTab | Cmd::GoTab(_) => {
                let at = self.shown_tab().index();
                let to = match cmd {
                    Cmd::NextTab => (at + 1) % 3,
                    Cmd::PrevTab => (at + 2) % 3,
                    Cmd::GoTab(n) => usize::from(n).min(2),
                    _ => at,
                };
                self.go_tab(Tab::ALL[to]);
            }
            Cmd::History => {
                if let Some(a) = self.selected_ref() {
                    // Cached runs show at once; the host's answer replaces them.
                    let runs = self.recent.get(&a).cloned();
                    let shown = self.panes.get(&a).and_then(|p| p.run_id.clone());
                    let index = runs
                        .as_ref()
                        .and_then(|r| r.iter().position(|x| Some(&x.run_id) == shown.as_ref()))
                        .unwrap_or(0);
                    self.modal = Modal::History {
                        action_ref: a.clone(),
                        runs,
                        error: None,
                        index,
                    };
                    let _ = self.io.read.send(Read::History(a));
                }
            }
            Cmd::Up | Cmd::Down | Cmd::PageUp | Cmd::PageDown | Cmd::Top | Cmd::Bottom => {
                self.navigate(cmd)
            }
            Cmd::Open => {
                let Some(item) = self.selected_item() else {
                    return;
                };
                let a = item.action_ref.clone();
                match self.open_kind(item) {
                    Some(OpenKind::Logs) => self.focus = Focus::Logs,
                    Some(OpenKind::Start) => self.intent(&a, Intent::Start),
                    Some(OpenKind::NeedsInput | OpenKind::Form) => self.intent(&a, Intent::Start),
                    None => {}
                }
            }
            Cmd::Toggle => {
                if let Some(item) = self.selected_item()
                    && let Some(i) = self.toggle_intent(item)
                {
                    let a = item.action_ref.clone();
                    self.intent(&a, i);
                }
            }
            Cmd::Restart => {
                if let Some(a) = self.selected_ref() {
                    self.intent(&a, Intent::Restart);
                }
            }
            Cmd::Focus => {
                self.focus = match self.focus {
                    Focus::List => Focus::Logs,
                    Focus::Logs => Focus::List,
                };
                self.refresh_branch(true);
            }
            Cmd::Search => {
                self.modal = Modal::Search {
                    logs: self.focus == Focus::Logs,
                    // A new search starts empty; Esc restores `prev_filter`.
                    text: String::new(),
                    prev_filter: self.filter.clone(),
                }
            }
            Cmd::Quit => self.quit = Some(Quit::Normal(None)),
            Cmd::Keep => {
                self.keeping = true;
                self.info("keeping the session in the background...");
                let _ = self.io.control.send(Control::Keep);
            }
            Cmd::Left => {
                if let Some(p) = self.selected_pane_mut() {
                    p.hscroll = p.hscroll.saturating_sub(8);
                }
            }
            Cmd::Right => {
                if let Some(p) = self.selected_pane_mut() {
                    p.hscroll += 8;
                }
            }
            Cmd::Wrap => {
                if let Some(p) = self.selected_pane_mut() {
                    p.wrap = !p.wrap;
                    // Keep the anchor record; its row offset no longer applies.
                    if let crate::logs::Viewport::Pinned { top, .. } = p.view {
                        p.view = crate::logs::Viewport::Pinned {
                            top,
                            line_offset: 0,
                        };
                    }
                }
            }
            Cmd::Copy => {
                if let Some((text, n)) = self.selected_pane().and_then(LogPane::selection_text) {
                    clip::copy(text, n, self.io.events.clone());
                }
            }
            Cmd::Select => {
                if let Some(p) = self.selected_pane_mut() {
                    if !p.is_pinned() {
                        let _ = p.move_cursor(0);
                    }
                    p.anchor = p.cursor;
                }
            }
            Cmd::Escape => {
                if self.focus == Focus::Logs {
                    let Some(a) = self.selected_ref() else {
                        return;
                    };
                    if let Some(p) = self.panes.get_mut(&a)
                        && p.anchor.is_some()
                    {
                        p.anchor = None;
                    } else if self.viewing.remove(&a).is_some() {
                        self.sync_pane(&a);
                        self.info(format!("showing the latest run of {a}"));
                    }
                } else {
                    self.filter.clear();
                    let keep = self.selected_key();
                    self.refilter(keep);
                    self.on_select();
                }
            }
            Cmd::NextMatch | Cmd::PrevMatch => {
                if let Some(q) = self.log_query.clone() {
                    self.find(&q, cmd == Cmd::NextMatch);
                }
            }
            Cmd::Help => self.modal = Modal::Help { top: 0, max: 0 },
            Cmd::Attach => {
                if let Some((a, run_id)) = self.attach_target() {
                    self.term.open(a, run_id, self.io.events.clone());
                }
            }
            Cmd::Detach | Cmd::Forward => {}
        }
    }

    /// Commands that act on a selected view. Returns false for global ones.
    fn exec_view(&mut self, cmd: Cmd) -> bool {
        let Some(v) = self.selected_view() else {
            return false;
        };
        let r = v.view_ref.clone();
        let focus = self.focus;
        match (cmd, focus) {
            (Cmd::Open, Focus::List) => self.focus = Focus::Logs,
            (Cmd::Open, Focus::Logs) => {
                let choices = self
                    .view_panes
                    .get(&r)
                    .map(|p| p.row_actions.clone())
                    .unwrap_or_default();
                match choices.len() {
                    0 => {}
                    1 => self.run_row_action(r, choices[0].clone()),
                    _ => {
                        self.modal = Modal::RowAction {
                            view_ref: r,
                            choices,
                            index: 0,
                        }
                    }
                }
            }
            // Esc leaves the view focus, like Tab.
            (Cmd::Escape, Focus::Logs) => self.focus = Focus::List,
            (Cmd::Restart, _) => {
                self.load_view(&r);
                self.info(format!("reading {r} again"));
            }
            (
                Cmd::Up | Cmd::Down | Cmd::PageUp | Cmd::PageDown | Cmd::Top | Cmd::Bottom,
                Focus::Logs,
            ) => {
                if let Some(p) = self.view_panes.get_mut(&r) {
                    match cmd {
                        Cmd::Up => p.move_by(-1),
                        Cmd::Down => p.move_by(1),
                        Cmd::PageUp => p.page(false),
                        Cmd::PageDown => p.page(true),
                        Cmd::Top => p.home(false),
                        _ => p.home(true),
                    }
                }
            }
            (Cmd::Left | Cmd::Right, Focus::Logs) => {
                if let Some(p) = self.view_panes.get_mut(&r) {
                    p.left_right(cmd == Cmd::Right);
                }
            }
            (Cmd::Wrap, Focus::Logs) => {
                if let Some(p) = self.view_panes.get_mut(&r) {
                    p.toggle_wrap();
                }
            }
            (Cmd::Copy | Cmd::CopyAll, Focus::Logs) => {
                let got = self.view_panes.get(&r).and_then(|p| {
                    if cmd == Cmd::Copy {
                        p.copy_value()
                    } else {
                        p.copy_whole()
                    }
                });
                if let Some((text, what)) = got {
                    let n = text.lines().count().max(1);
                    self.info(format!("copying the {what}..."));
                    clip::copy(text, n, self.io.events.clone());
                }
            }
            _ => return false,
        }
        true
    }

    /// Mouse mode: the wheel scrolls whatever has the focus.
    pub fn mouse_event(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::MouseEventKind;
        if !self.mouse || !matches!(self.modal, Modal::None) {
            return;
        }
        let cmd = match m.kind {
            MouseEventKind::ScrollDown => Cmd::Down,
            MouseEventKind::ScrollUp => Cmd::Up,
            _ => return,
        };
        for _ in 0..3 {
            self.exec(cmd);
        }
    }

    /// Bracketed paste goes to the text field that has the focus.
    pub fn paste(&mut self, text: &str) {
        match &mut self.modal {
            Modal::Form(f) => f.paste(text),
            Modal::Command { text: t, .. } => t.push_str(&text.replace(['\r', '\n'], " ")),
            Modal::Search { .. } => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                }
            }
            _ => {}
        }
    }

    fn navigate(&mut self, cmd: Cmd) {
        match self.focus {
            Focus::List => {
                let n = self.visible.len();
                if n == 0 {
                    return;
                }
                let page = 10;
                self.selected = match cmd {
                    Cmd::Up => self.selected.saturating_sub(1),
                    Cmd::Down => (self.selected + 1).min(n - 1),
                    Cmd::PageUp => self.selected.saturating_sub(page),
                    Cmd::PageDown => (self.selected + page).min(n - 1),
                    Cmd::Top => 0,
                    _ => n - 1,
                };
                self.on_select();
            }
            Focus::Logs if self.tab == Tab::Output => {
                self.output_top = match cmd {
                    Cmd::Up => self.output_top.saturating_sub(1),
                    Cmd::Down => self.output_top + 1,
                    Cmd::PageUp => self.output_top.saturating_sub(10),
                    Cmd::PageDown => self.output_top + 10,
                    Cmd::Top => 0,
                    // Drawing clamps this to the last page.
                    _ => usize::MAX / 2,
                };
            }
            Focus::Logs => {
                let Some(p) = self.selected_pane_mut() else {
                    return;
                };
                let page = p.height.saturating_sub(1).max(1) as isize;
                let at_top = match cmd {
                    Cmd::Up => p.move_cursor(-1),
                    Cmd::Down => p.move_cursor(1),
                    Cmd::PageUp => p.page(-page),
                    Cmd::PageDown => p.page(page),
                    Cmd::Top => {
                        p.top();
                        true
                    }
                    _ => {
                        p.follow();
                        false
                    }
                };
                // Reaching the first loaded record pages older history in, if the host has it.
                if at_top
                    && !p.loading_older
                    && let (Some(cursor), Some(run_id)) = (p.older_cursor.clone(), p.run_id.clone())
                {
                    p.loading_older = true;
                    let _ = self.io.read.send(Read::Older { run_id, cursor });
                }
            }
        }
    }
}

/// How well one catalog entry matches the filter words, like `mira` catalog search:
/// `None` when no word matches; otherwise the best tier over all words (0 exact ref or ID,
/// 1 exact title, 2 ref/ID/title prefix, 3 ref/ID/title substring, 4 tag, 5 description),
/// then more matched words first. `extra_tags` match like tags (a view's kind word).
fn rank(
    words: &[String],
    item_ref: &str,
    id: &str,
    title: &str,
    tags: &[String],
    extra_tags: &[String],
    description: &str,
) -> Option<(u8, std::cmp::Reverse<usize>)> {
    if words.is_empty() {
        return Some((0, std::cmp::Reverse(0)));
    }
    let (item_ref, id, title) = (
        item_ref.to_lowercase(),
        id.to_lowercase(),
        title.to_lowercase(),
    );
    let description = description.to_lowercase();
    let names = [&item_ref, &id, &title];
    let tier = |w: &str| -> Option<u8> {
        if item_ref == w || id == w {
            Some(0)
        } else if title == w {
            Some(1)
        } else if names.iter().any(|n| n.starts_with(w)) {
            Some(2)
        } else if names.iter().any(|n| n.contains(w)) {
            Some(3)
        } else if tags
            .iter()
            .chain(extra_tags)
            .any(|t| t.to_lowercase().contains(w))
        {
            Some(4)
        } else if description.contains(w) {
            Some(5)
        } else {
            None
        }
    };
    let tiers: Vec<u8> = words.iter().filter_map(|w| tier(w)).collect();
    let best = tiers.iter().min()?;
    Some((*best, std::cmp::Reverse(tiers.len())))
}

/// Lifecycle stages a run notice outlives: a started run stays "started" while it runs.
fn stage(l: Lifecycle) -> u8 {
    match l {
        Lifecycle::Starting | Lifecycle::Running => 0,
        Lifecycle::Stopping { .. } => 1,
        Lifecycle::Finished { .. } => 2,
    }
}

/// The non-empty `name` of the workspace file, if it has one.
fn workspace_name(root: &str) -> Option<String> {
    let path = std::path::Path::new(root)
        .join(".mira")
        .join("workspace.json");
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
}

fn start_word(item: &Item) -> &'static str {
    match item.mode {
        ActionMode::Process => "start",
        ActionMode::Task => "run",
    }
}

fn inputs_of(schema: Option<&JsonObject>) -> Inputs {
    match schema {
        Some(s) if form::has_fields(s) => Inputs::Form(s.clone()),
        _ => Inputs::Free,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let (control, _) = tokio::sync::mpsc::unbounded_channel();
        let (read, _) = tokio::sync::mpsc::unbounded_channel();
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        let root = mira_protocol::ids::AbsolutePath::parse("/tmp/mira-tui-test".into()).unwrap();
        let paths = mira_protocol::paths::WorkspacePaths::new(root);
        App::new(
            "/tmp/mira-tui-test".into(),
            time::UtcOffset::UTC,
            Io {
                control,
                read,
                events,
                paths,
            },
        )
    }

    fn key(a: &mut App, code: KeyCode) {
        a.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn add_view(a: &mut App, r: &str, kind: ViewKind) {
        let view_ref: ViewRef = r.parse().unwrap();
        let plugin = view_ref.plugin.to_string();
        if !a.plugin_order.contains(&plugin) {
            a.plugin_order.push(plugin);
        }
        a.views.push(ViewItem {
            view_ref,
            title: r.into(),
            description: String::new(),
            tags: Vec::new(),
            kind,
        });
        a.refilter(None);
    }

    fn add_action(a: &mut App, r: &str, title: &str, tags: &[&str], description: &str) {
        let action_ref: ActionRef = r.parse().unwrap();
        let plugin = action_ref.plugin.to_string();
        if !a.plugin_order.contains(&plugin) {
            a.plugin_order.push(plugin);
        }
        a.items.push(Item {
            action_ref,
            title: title.into(),
            description: description.into(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            mode: ActionMode::Task,
            enabled: true,
            definition_hash: Digest::of_bytes(r.as_bytes()),
        });
        a.refilter(None);
    }

    fn search(a: &mut App, text: &str) -> Vec<String> {
        key(a, KeyCode::Char('/'));
        for c in text.chars() {
            key(a, KeyCode::Char(c));
        }
        key(a, KeyCode::Enter);
        a.visible
            .iter()
            .filter_map(|&e| a.entry_key(e).map(|k| k.to_string()))
            .collect()
    }

    #[test]
    fn filter_keeps_matches_grouped_under_one_plugin_heading() {
        let mut a = app();
        add_action(&mut a, "dev.check", "Check", &[], "Run every check.");
        add_action(&mut a, "dev.env", "Environment check", &[], "Show env.");
        add_action(&mut a, "files.check", "Check files", &[], "Check files.");
        add_action(&mut a, "dev.fail", "Failing check", &[], "Fails.");
        let found = search(&mut a, "check");
        assert_eq!(found[0], "dev.check", "the best match stays first");
        let plugins: Vec<&str> = found.iter().map(|r| r.split('.').next().unwrap()).collect();
        let mut runs = plugins.clone();
        runs.dedup();
        let mut unique = runs.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            runs.len(),
            unique.len(),
            "each plugin appears in one block: {found:?}"
        );
    }

    #[test]
    fn filter_ranks_the_best_match_first() {
        let mut a = app();
        add_action(
            &mut a,
            "dev.typecheck",
            "Typecheck",
            &[],
            "Run the type checker.",
        );
        add_action(&mut a, "dev.check", "Check", &[], "Run every check.");
        add_action(&mut a, "dev.lint", "Lint", &[], "Lint the code.");
        add_action(
            &mut a,
            "style.fix",
            "Fix style",
            &["lint"],
            "Formats files.",
        );
        add_action(&mut a, "dev.test", "Test", &[], "Run the tests.");
        assert_eq!(search(&mut a, "lint"), ["dev.lint", "style.fix"]);
        assert_eq!(a.selected_ref().unwrap().to_string(), "dev.lint");
        assert_eq!(search(&mut a, "check"), ["dev.check", "dev.typecheck"]);
        assert_eq!(a.selected_ref().unwrap().to_string(), "dev.check");
        // Any word matches; more matched words rank higher within a tier.
        assert_eq!(
            search(&mut a, "test tests"),
            ["dev.test"],
            "one entry matches both words"
        );
        assert_eq!(search(&mut a, "type every"), ["dev.typecheck", "dev.check"]);
        // Ties keep the list order.
        assert_eq!(
            search(&mut a, "run"),
            ["dev.typecheck", "dev.check", "dev.test"]
        );
    }

    fn run(a: &str, id: &RunId, lifecycle: Lifecycle) -> RunSummary {
        RunSummary {
            run_id: id.clone(),
            action_ref: Some(a.parse().unwrap()),
            lifecycle,
            reported_health: mira_protocol::run::ReportedHealth::unknown(),
            definition_hash: Digest::of_bytes(a.as_bytes()),
            started_at: Timestamp::now(),
        }
    }

    #[test]
    fn run_notices_go_when_the_run_moves_on() {
        let mut a = app();
        add_action(&mut a, "dev.web", "Web", &[], "");
        add_action(&mut a, "dev.lint", "Lint", &[], "");
        let web: ActionRef = "dev.web".parse().unwrap();
        let id: RunId = "r_0000000000004000800000000000000a".parse().unwrap();
        a.apply_runs(None, vec![run("dev.web", &id, Lifecycle::Starting)]);
        a.info_run(&web, &id, "started dev.web");
        // Starting to running keeps the notice.
        a.apply_runs(None, vec![run("dev.web", &id, Lifecycle::Running)]);
        assert!(a.notice.is_some());
        // Another selected item drops it.
        key(&mut a, KeyCode::Char('j'));
        assert!(a.notice.is_none());
        key(&mut a, KeyCode::Char('k'));
        let stopping = Lifecycle::Stopping {
            reason: mira_protocol::run::StopReason::User,
        };
        a.apply_runs(None, vec![run("dev.web", &id, stopping)]);
        a.info_run(&web, &id, "stopping dev.web");
        assert!(a.notice.is_some());
        // The run ended.
        a.apply_runs(None, vec![]);
        assert!(a.notice.is_none());
        // Errors stay.
        a.error("dev.web did not stop");
        a.apply_runs(None, vec![]);
        key(&mut a, KeyCode::Char('j'));
        assert!(a.notice.is_some());
    }

    #[test]
    fn a_task_form_runs_again_and_a_process_form_restarts() {
        let mut a = app();
        add_action(&mut a, "dev.test", "Test", &[], "");
        let r: ActionRef = "dev.test".parse().unwrap();
        let schema: JsonObject = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {"only": {"type": "string"}}
        }))
        .unwrap();
        let hash = a.items[0].definition_hash.clone();
        a.inputs.insert(r.clone(), (hash, Inputs::Form(schema)));
        a.last.insert(
            r,
            LastRun {
                run_id: "r_0000000000004000800000000000000a".parse().unwrap(),
                lifecycle: Lifecycle::Finished {
                    outcome: mira_protocol::run::Outcome::Succeeded,
                },
                exit: None,
                started_at: None,
                ended_at: None,
                result: None,
            },
        );
        key(&mut a, KeyCode::Char('r'));
        let enter = |a: &App| {
            a.bindings()
                .into_iter()
                .find(|b| b.keys == "Enter")
                .map(|b| b.label)
        };
        assert_eq!(enter(&a).as_deref(), Some("run"));
        a.items[0].mode = ActionMode::Process;
        assert_eq!(enter(&a).as_deref(), Some("restart"));
    }

    #[test]
    fn esc_leaves_the_view_focus() {
        let mut a = app();
        add_view(&mut a, "dev.grid", ViewKind::Table);
        key(&mut a, KeyCode::Enter);
        assert!(a.focus == Focus::Logs);
        assert!(
            a.bindings()
                .iter()
                .any(|b| b.footer && b.keys == "Esc" && b.label == "back")
        );
        key(&mut a, KeyCode::Esc);
        assert!(a.focus == Focus::List);
    }
}
