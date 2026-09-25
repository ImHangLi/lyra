//! Presentation state and the single key router (§12.3). The host owns every fact; this
//! state is a projection of `state`/`log` events plus replies, and every action goes through
//! the typed client. Footer and key handling read the same [`App::bindings`] set.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use lyra_protocol::error::ErrorInfo;
use lyra_protocol::ids::{ActionId, ActionRef, Digest, ItemRef, RunId, SessionId, ViewRef};
use lyra_protocol::ipc::*;
use lyra_protocol::manifest::{ActionMode, JsonObject, ViewKind};
use lyra_protocol::run::{ExitInfo, Lifecycle, RunRecord, RunSummary};
use lyra_protocol::time::Timestamp;
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
const NOTICE_SECS: u64 = 8;

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
    Help,
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
    pub ended_at: Option<Timestamp>,
}

pub struct Notice {
    pub text: String,
    pub error: bool,
    at: Instant,
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
    pub paths: lyra_protocol::paths::WorkspacePaths,
}

pub struct App {
    pub root: String,
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
    /// The PTY attach view (§13).
    pub term: Terminals,
    io: Io,
}

impl App {
    pub fn new(root: String, offset: time::UtcOffset, io: Io) -> Self {
        Self {
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
                    self.active.insert(a, r);
                }
                None => self.adhoc_runs += 1,
            }
        }
        for (a, r) in old {
            if self.active.get(&a).is_some_and(|n| n.run_id == r.run_id) {
                continue;
            }
            // The run ended: fetch its final record for the outcome.
            self.last.insert(
                a.clone(),
                LastRun {
                    run_id: r.run_id.clone(),
                    lifecycle: r.lifecycle,
                    exit: None,
                    ended_at: None,
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

    pub fn handle(&mut self, ev: Event) {
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
                        self.info(what);
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
                    Ok(s) => self.info(format!("stopping {a} ({})", s.run_id)),
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
                Err(e) if e.code == lyra_protocol::ErrorCode::VIEW_CHANGED => {
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
                ended_at: rec.ended_at,
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
                if e.code != lyra_protocol::ErrorCode::NOT_FOUND {
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
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.at.elapsed().as_secs() >= NOTICE_SECS)
        {
            self.notice = None;
        }
    }

    // ----- notices ------------------------------------------------------------------

    pub fn info(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: false,
            at: Instant::now(),
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: true,
            at: Instant::now(),
        });
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

    fn refilter(&mut self, keep: Option<ItemRef>) {
        let words: Vec<String> = self
            .filter
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let hit = |hay: String| {
            let hay = hay.to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        };
        let mut out = Vec::new();
        for p in &self.plugin_order {
            for (n, i) in self.items.iter().enumerate() {
                if i.action_ref.plugin.as_str() == p
                    && hit(format!(
                        "{} {} {} {}",
                        i.action_ref,
                        i.title,
                        i.description,
                        i.tags.join(" ")
                    ))
                {
                    out.push(Entry::Action(n));
                }
            }
            for (n, v) in self.views.iter().enumerate() {
                if v.view_ref.plugin.as_str() == p
                    && hit(format!(
                        "{} {} {} {} {}",
                        v.view_ref,
                        v.title,
                        v.description,
                        v.tags.join(" "),
                        crate::views::kind_word(v.kind)
                    ))
                {
                    out.push(Entry::View(n));
                }
            }
        }
        self.visible = out;
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

    /// Makes the action's panel show its current (or latest) run.
    fn sync_pane(&mut self, a: &ActionRef) {
        let want = self
            .active
            .get(a)
            .map(|r| r.run_id.clone())
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
                    let w = match f.intent {
                        Intent::Restart => "restart",
                        _ => "run",
                    };
                    v.push(bind("Enter", w, Cmd::Open));
                }
                v.push(bind("Esc", "cancel", Cmd::Escape));
                return v;
            }
            Modal::Command { .. } => {
                v.push(bind("Enter", "run lyra command", Cmd::Open));
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
            Modal::Help => {
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
        let pane = self.selected_pane().filter(|p| !p.records.is_empty());
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
                    "Work continues under a background lease without expiry ({runs} active run(s)); `lyra down` stops it."
                )
            };
        }
        if let Some(t) = s.expires_at {
            return if short {
                format!("kept until {}", self.clock(t, false))
            } else {
                format!(
                    "Work continues in the background until {} ({runs} active run(s)); `lyra down` stops it.",
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
                            self.filter = text;
                            let keep = self.selected_key();
                            self.refilter(keep);
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
                            let t = format!("lyra {}", words.join(" "));
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
            Modal::Help => {
                if matches!(
                    k.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
                ) || (ctrl && k.code == KeyCode::Char('c'))
                {
                    self.modal = Modal::None;
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
        if let Modal::Search { logs, text, .. } = std::mem::replace(&mut self.modal, Modal::None)
            && logs
        {
            if text.is_empty() {
                self.log_query = None;
                return;
            }
            self.log_query = Some(text.clone());
            self.find(&text, true);
        }
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
                }
            }
            Cmd::Search => {
                self.modal = Modal::Search {
                    logs: self.focus == Focus::Logs,
                    text: if self.focus == Focus::List {
                        self.filter.clone()
                    } else {
                        String::new()
                    },
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
                    if let Some(p) = self.selected_pane_mut() {
                        p.anchor = None;
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
            Cmd::Help => self.modal = Modal::Help,
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
