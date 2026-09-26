//! Rendering (§12.1): status line, tool list, detail and log panel, notice line, footer.
//! Only visible rows are built. State is never shown by color alone.

use lyra_protocol::ipc::{SessionMode, SessionState};
use lyra_protocol::manifest::ActionMode;
use lyra_protocol::run::{Lifecycle, LogStream, Outcome};
use lyra_protocol::time::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::{App, Cmd, Entry, Focus, Inputs, Intent, Item, Modal, ViewItem};
use crate::form::Kind;
use crate::logs::{cells, display, slice_cells};
use crate::views::{durability_word, freshness_word, kind_word};

pub const MIN_W: u16 = 60;
pub const MIN_H: u16 = 18;

struct Theme {
    color: bool,
}

impl Theme {
    fn fg(&self, c: Color) -> Style {
        if self.color {
            Style::default().fg(c)
        } else {
            Style::default()
        }
    }
    fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
    fn dim(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }
}

pub fn draw(f: &mut Frame, app: &mut App, color: bool) {
    let t = Theme { color };
    let area = f.area();
    if area.width < MIN_W || area.height < MIN_H {
        let msg = vec![
            Line::from(Span::styled("Window too small", t.bold())),
            Line::from(format!(
                "Lyra needs at least {MIN_W}x{MIN_H}; this window is {}x{}.",
                area.width, area.height
            )),
            Line::from("Resize to continue. q quits."),
        ];
        f.render_widget(Paragraph::new(msg), area);
        return;
    }
    let [header, body, status, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(f, app, &t, header);
    app.narrow = area.width < 80;
    if app.term.is_open() {
        crate::terminal::draw(f, &mut app.term, body, color);
    } else if app.narrow {
        match app.focus {
            Focus::List => draw_list(f, app, &t, body),
            Focus::Logs => draw_detail(f, app, &t, body),
        }
    } else {
        let list_w = if area.width >= 100 { 34 } else { 27 };
        let [list, detail] =
            Layout::horizontal([Constraint::Length(list_w), Constraint::Min(1)]).areas(body);
        let block = Block::new().borders(Borders::RIGHT);
        let inner = block.inner(list);
        f.render_widget(block, list);
        draw_list(f, app, &t, inner);
        let detail = Rect {
            x: detail.x + 1,
            width: detail.width.saturating_sub(1),
            ..detail
        };
        draw_detail(f, app, &t, detail);
    }
    draw_status(f, app, &t, status);
    draw_footer(f, app, &t, footer);
    match &app.modal {
        Modal::Help => draw_help(f, app, &t, area),
        Modal::Form(_) => draw_form(f, app, &t, area),
        Modal::Output(_) => draw_output(f, app, &t, area),
        Modal::RowAction { .. } => draw_row_actions(f, app, &t, area),
        Modal::History { .. } => draw_history(f, app, &t, area),
        _ => {}
    }
}

fn short_root(root: &str, max: usize) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let r = if !home.is_empty() && root.starts_with(&home) {
        format!("~{}", &root[home.len()..])
    } else {
        root.to_owned()
    };
    if cells(&r) <= max {
        return r;
    }
    let tail: Vec<&str> = r.rsplit('/').take(2).collect();
    let s = format!(
        ".../{}/{}",
        tail.get(1).unwrap_or(&""),
        tail.first().unwrap_or(&"")
    );
    slice_cells(&s, 0, max)
}

/// Longest branch name the header shows before it cuts the end.
const BRANCH_MAX: usize = 24;
/// Shortest branch and path the header keeps when both have to shrink.
const BRANCH_MIN: usize = 12;
const ROOT_MIN: usize = 8;

/// Cuts `s` to `max` cells, marking the cut with `...`.
fn clip_end(s: &str, max: usize) -> String {
    if cells(s) <= max {
        return s.to_owned();
    }
    if max <= 3 {
        return slice_cells(s, 0, max);
    }
    format!("{}...", slice_cells(s, 0, max - 3))
}

fn draw_header(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let mut right = match &app.session {
        None => "NO SESSION".to_owned(),
        Some(s) if s.state == SessionState::Stopping => "STOPPING".to_owned(),
        Some(s) => {
            let mode = match s.mode {
                SessionMode::Foreground => "FOREGROUND",
                SessionMode::Background => "BACKGROUND",
            };
            let mut x = format!(
                "{mode} · {} controller{}",
                s.controller_count,
                if s.controller_count == 1 { "" } else { "s" }
            );
            if let Some(e) = s.expires_at {
                let left = (e.unix_ms() - Timestamp::now().unix_ms()) / 1000;
                x.push_str(&format!(" · background until {}", app.clock(e, false)));
                if (0..=60).contains(&left) {
                    x.push_str(&format!(" (ends in {left}s)"));
                }
            }
            x
        }
    };
    if !app.is_controller() && app.session.is_some() {
        right.push_str(" · observing");
    }
    let warnings = app.storage_warnings.len() + app.config_warnings.len();
    if warnings > 0 {
        right.push_str(&format!(" · {warnings} warning(s)"));
    }
    let rw = cells(&right);
    let left_max = (area.width as usize).saturating_sub(rw + 8);
    // The path shrinks first; the branch shrinks only to leave the path its minimum.
    let cap = left_max
        .saturating_sub(4 + ROOT_MIN)
        .clamp(BRANCH_MIN, BRANCH_MAX)
        .min(left_max.saturating_sub(2));
    let branch = app
        .branch
        .as_deref()
        .map(|b| format!("({})", clip_end(&display(b), cap)))
        .filter(|b| b.len() > 2)
        .unwrap_or_default();
    let root = match left_max.checked_sub(cells(&branch) + 2) {
        _ if branch.is_empty() => short_root(&app.root, left_max),
        Some(m) if m >= ROOT_MIN => short_root(&app.root, m),
        _ => String::new(),
    };
    let sep = if root.is_empty() || branch.is_empty() {
        ""
    } else {
        "  "
    };
    let gap =
        (area.width as usize).saturating_sub(6 + cells(&root) + sep.len() + cells(&branch) + rw);
    let line = Line::from(vec![
        Span::styled("Lyra", t.bold()),
        Span::raw("  "),
        Span::raw(root),
        Span::raw(sep),
        Span::styled(branch, t.fg(Color::Cyan)),
        Span::raw(" ".repeat(gap.max(1))),
        Span::styled(right, t.bold()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn state_label(app: &App, t: &Theme, item: &Item) -> (String, Style) {
    if let Some(i) = app.pending.get(&item.action_ref) {
        let s = match i {
            Intent::Stop => "stop...",
            _ => "start...",
        };
        return (s.into(), t.fg(Color::Yellow));
    }
    if let Some(r) = app.active.get(&item.action_ref) {
        return match r.lifecycle {
            Lifecycle::Starting => ("starting".into(), t.fg(Color::Yellow)),
            Lifecycle::Running => ("running".into(), t.fg(Color::Green)),
            Lifecycle::Stopping { .. } => ("stopping".into(), t.fg(Color::Yellow)),
            Lifecycle::Finished { .. } => ("finished".into(), Style::default()),
        };
    }
    if !item.enabled {
        return ("disabled".into(), t.dim());
    }
    match app.last.get(&item.action_ref).map(|l| l.lifecycle) {
        Some(Lifecycle::Finished { outcome }) => match outcome {
            Outcome::Succeeded => ("ok".into(), t.fg(Color::Green)),
            Outcome::Failed => ("failed".into(), t.fg(Color::Red)),
            Outcome::Cancelled => ("stopped".into(), t.dim()),
            Outcome::TimedOut => ("timeout".into(), t.fg(Color::Red)),
            Outcome::Interrupted => ("interrupted".into(), t.fg(Color::Red)),
        },
        Some(_) => ("ended".into(), t.dim()),
        None => (String::new(), Style::default()),
    }
}

fn view_label(app: &App, t: &Theme, v: &ViewItem) -> (String, Style) {
    let Some(p) = app.view_panes.get(&v.view_ref) else {
        return (String::new(), Style::default());
    };
    match (&p.meta, p.revision()) {
        (_, _) if p.error.is_some() => ("error".into(), t.fg(Color::Red)),
        (None, _) => ("...".into(), t.dim()),
        (Some(_), None) => ("no data".into(), t.dim()),
        (Some(m), Some(_)) => match m.freshness {
            lyra_protocol::view::Freshness::Current => ("current".into(), t.fg(Color::Green)),
            lyra_protocol::view::Freshness::Historical => ("hist".into(), t.dim()),
            lyra_protocol::view::Freshness::Stale => ("stale".into(), t.fg(Color::Yellow)),
        },
    }
}

fn draw_list(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    let w = area.width as usize;
    let mut rows: Vec<Line> = Vec::new();
    if !app.filter.is_empty() {
        rows.push(Line::from(Span::styled(
            slice_cells(
                &format!(
                    "/{}  ({} of {})",
                    app.filter,
                    app.visible.len(),
                    app.items.len() + app.views.len()
                ),
                0,
                w,
            ),
            t.dim(),
        )));
    }
    if app.items.is_empty() && app.views.is_empty() {
        let msg = match &app.catalog_error {
            Some(e) => format!("Catalog unavailable: [{}] {}", e.code, e.message),
            None => "No actions in this workspace.".into(),
        };
        f.render_widget(
            Paragraph::new(msg).wrap(ratatui::widgets::Wrap { trim: true }),
            area,
        );
        return;
    }
    if app.visible.is_empty() {
        rows.push(Line::from("No tool matches the filter."));
    }
    let mut sel_row = 0usize;
    let mut last_plugin: Option<String> = None;
    for (vi, &entry) in app.visible.iter().enumerate() {
        let (plugin, title, (state, style)) = match entry {
            Entry::Action(i) => {
                let item = &app.items[i];
                (
                    item.action_ref.plugin.to_string(),
                    item.title.clone(),
                    state_label(app, t, item),
                )
            }
            Entry::View(i) => {
                let v = &app.views[i];
                (
                    v.view_ref.plugin.to_string(),
                    format!("[{}] {}", kind_word(v.kind), v.title),
                    view_label(app, t, v),
                )
            }
        };
        if last_plugin.as_deref() != Some(plugin.as_str()) {
            rows.push(Line::from(Span::styled(
                slice_cells(&plugin.to_uppercase(), 0, w),
                t.bold(),
            )));
            last_plugin = Some(plugin);
        }
        let selected = vi == app.selected;
        if selected {
            sel_row = rows.len();
        }
        let sw = cells(&state);
        let marker = if selected { "> " } else { "  " };
        let title_w = w.saturating_sub(2 + sw + 1);
        let title = slice_cells(&display(&title), 0, title_w);
        let pad = w.saturating_sub(2 + cells(&title) + sw);
        let base = if selected && app.focus == Focus::List {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        rows.push(Line::from(vec![
            Span::styled(format!("{marker}{title}{}", " ".repeat(pad)), base),
            Span::styled(state, base.patch(style)),
        ]));
    }
    let h = area.height as usize;
    if sel_row < app.list_offset {
        // Show the group header above the first item of a group when possible.
        app.list_offset = sel_row.saturating_sub(1);
    } else if sel_row >= app.list_offset + h {
        app.list_offset = sel_row + 1 - h;
    }
    app.list_offset = app.list_offset.min(rows.len().saturating_sub(1));
    let visible: Vec<Line> = rows.into_iter().skip(app.list_offset).take(h).collect();
    f.render_widget(Paragraph::new(visible), area);
}

fn ago(ts: Timestamp) -> String {
    let s = ((Timestamp::now().unix_ms() - ts.unix_ms()) / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}

fn draw_detail(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    if app.selected_view().is_some() {
        draw_view(f, app, t, area);
        return;
    }
    let Some(item) = app.selected_item() else {
        f.render_widget(Paragraph::new("Select a tool on the left."), area);
        return;
    };
    let w = area.width as usize;
    let a = item.action_ref.clone();
    let mode = match item.mode {
        ActionMode::Process => "process",
        ActionMode::Task => "task",
    };
    let mut l1 = vec![
        Span::styled(slice_cells(&item.title, 0, w / 2), t.bold()),
        Span::raw(format!("  {a}  {mode}")),
    ];
    if !item.enabled {
        l1.push(Span::raw("  (disabled)"));
    }
    let state = if let Some(r) = app.active.get(&a) {
        let life = match r.lifecycle {
            Lifecycle::Starting => "starting".to_owned(),
            Lifecycle::Running => "running".to_owned(),
            Lifecycle::Stopping { reason } => {
                format!(
                    "stopping ({})",
                    serde_json::to_value(reason)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default()
                )
            }
            Lifecycle::Finished { .. } => "finished".to_owned(),
        };
        let health = serde_json::to_value(r.reported_health.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        format!(
            "{life} · started {} ({} ago) · health {health}",
            app.clock(r.started_at, true),
            ago(r.started_at)
        )
    } else if let Some(l) = app.last.get(&a) {
        let what = match l.lifecycle {
            Lifecycle::Finished { outcome } => match outcome {
                Outcome::Succeeded => "succeeded",
                Outcome::Failed => "failed",
                Outcome::Cancelled => "was stopped",
                Outcome::TimedOut => "timed out",
                Outcome::Interrupted => "was interrupted",
            },
            _ => "ended",
        };
        let exit = l
            .exit
            .as_ref()
            .map_or(String::new(), |e| match (e.code, &e.signal) {
                (Some(c), _) => format!(" (exit {c})"),
                (None, Some(s)) => format!(" ({s})"),
                _ => String::new(),
            });
        let when = l
            .ended_at
            .map_or(String::new(), |e| format!(" at {}", app.clock(e, true)));
        format!("last run {what}{exit}{when} · {}", l.run_id)
    } else {
        "not running".to_owned()
    };
    let inputs = match app.inputs.get(&a).map(|(_, i)| i) {
        Some(Inputs::Form(s)) => {
            let req = crate::form::required_names(s);
            if req.is_empty() {
                " · input form (optional fields)".to_owned()
            } else {
                format!(" · input form, required: {}", req.join(", "))
            }
        }
        Some(Inputs::Unknown(m)) => format!(" · inputs unknown: {m}"),
        _ => String::new(),
    };
    let sched = if app.has_schedule(&a) && app.schedule_of(&a).is_none() {
        " · schedule off (never switched on; t turns it on)".to_owned()
    } else {
        String::new()
    };
    let sched = app.schedule_of(&a).map_or(sched, |sc| {
        let every = sc.every_ms / 1000;
        let every = if every >= 60 && every % 60 == 0 {
            format!("{}m", every / 60)
        } else {
            format!("{every}s")
        };
        let next = sc
            .next_at
            .map_or(String::new(), |n| format!(", next {}", app.clock(n, true)));
        let missed = if sc.missed_ticks > 0 {
            format!(", {} skipped tick(s)", sc.missed_ticks)
        } else {
            String::new()
        };
        format!(
            " · schedule {} every {every}{next}{missed}",
            if sc.enabled { "ON" } else { "off" }
        )
    });
    let l2 = format!("{state}{inputs}{sched}");
    let l3 = display(&item.description);
    let [head, bar, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(l1),
            Line::from(slice_cells(&l2, 0, w)),
            Line::from(Span::styled(slice_cells(&l3, 0, w), t.dim())),
        ]),
        head,
    );
    let gutter = if w >= 50 { 11 } else { 2 };
    let text_w = w.saturating_sub(gutter).max(1);
    let focus = app.focus == Focus::Logs;
    let offset = app.offset;
    let old = app.viewing.get(&a).map(|rec| {
        let when = rec
            .ended_at
            .map_or(String::new(), |e| format!(" at {}", app.clock(e, true)));
        (
            rec.run_id.clone(),
            format!(
                "{} RUN · {}{when}",
                freshness_of(rec).to_uppercase(),
                run_word(rec.lifecycle)
            ),
        )
    });
    let Some(p) = app.panes.get_mut(&a) else {
        f.render_widget(Paragraph::new("loading..."), body);
        return;
    };
    p.height = body.height as usize;
    p.width = text_w;
    // Status bar of the log panel.
    let mut bar_text = String::from("LOGS");
    // An older run chosen in the history list reads as history, never as the current run.
    if let Some(rec) = old.as_ref().filter(|o| p.run_id.as_ref() == Some(&o.0)) {
        bar_text.push_str(&format!(" · {} ·", rec.1));
    }
    if let Some(r) = &p.run_id {
        bar_text.push_str(&format!(" {r}"));
    }
    if p.is_pinned() {
        bar_text.push_str(&format!(" · PINNED · {} newer below", p.below()));
    } else {
        bar_text.push_str(" · FOLLOW");
    }
    if p.wrap {
        bar_text.push_str(" · wrap");
    } else if p.hscroll > 0 {
        bar_text.push_str(&format!(" · col +{}", p.hscroll));
    }
    if p.loading || p.loading_older {
        bar_text.push_str(" · loading...");
    }
    if p.lost_anchor || p.trimmed {
        bar_text.push_str(" · older lines trimmed from this view");
    }
    if let Some((a, b)) = p.gap {
        bar_text.push_str(&format!(" · #{a}-#{b} skipped by a stream reset"));
    }
    if p.history_gone()
        && let Some(first) = p.first_available
    {
        bar_text.push_str(&format!(
            " · earlier output no longer available (starts at #{first})"
        ));
    }
    let bar_style = if focus {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::UNDERLINED)
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("{:<w$}", slice_cells(&bar_text, 0, w), w = w),
            bar_style,
        )),
        bar,
    );
    if p.records.is_empty() {
        let msg = if let Some(e) = &p.error {
            format!("Cannot read logs: {e}")
        } else if p.loading {
            "loading...".to_owned()
        } else if p.run_id.is_none() {
            "No runs yet. Press Enter or s to start it.".to_owned()
        } else {
            "(no output yet)".to_owned()
        };
        f.render_widget(Paragraph::new(msg), body);
        return;
    }
    let cursor = if focus && p.is_pinned() {
        p.cursor_at()
    } else {
        None
    };
    let rows = p.visible();
    let mut lines = Vec::with_capacity(rows.len());
    for (idx, row) in rows {
        let r = &p.records[idx];
        let text = display(&r.text);
        let part = if p.wrap {
            slice_cells(&text, row * text_w, text_w)
        } else {
            slice_cells(&text, p.hscroll, text_w)
        };
        let mut style = Style::default();
        let err = matches!(r.stream, LogStream::Stderr);
        if matches!(r.stream, LogStream::Host | LogStream::Plugin) {
            style = style.add_modifier(Modifier::DIM);
        }
        if p.in_selection(idx) && (p.anchor.is_some() || Some(idx) == cursor) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let mut spans = Vec::with_capacity(3);
        if gutter > 2 {
            let clock = if row == 0 {
                let nanos = i128::from(r.recorded_at.unix_ms()) * 1_000_000;
                time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
                    .map(|d| {
                        let d = d.to_offset(offset);
                        format!("{:02}:{:02}:{:02}", d.hour(), d.minute(), d.second())
                    })
                    .unwrap_or_else(|_| "--:--:--".into())
            } else {
                "        ".into()
            };
            spans.push(Span::styled(clock, t.dim()));
        }
        let tag = match (row, r.stream) {
            (0, LogStream::Stderr) => "! ",
            (0, LogStream::Host) => "* ",
            _ => "  ",
        };
        spans.push(Span::styled(
            if gutter > 2 {
                format!(" {tag}")
            } else {
                tag.to_owned()
            },
            if err { t.fg(Color::Red) } else { t.dim() },
        ));
        spans.push(Span::styled(part, style));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), body);
}

fn draw_status(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let w = area.width as usize;
    let (text, style) = match &app.modal {
        Modal::Search { logs, text, .. } => {
            let what = if *logs {
                "find in logs"
            } else {
                "filter tools"
            };
            (format!("{what}: /{text}_"), t.bold())
        }
        Modal::Command { text, error } => match error {
            Some(e) => (format!(":{text}_   {e}"), t.bold()),
            None => (
                format!(":{text}_   {}", crate::cmdbar::hint(text)),
                t.bold(),
            ),
        },
        _ => {
            if let Some(n) = &app.notice {
                (
                    n.text.clone(),
                    if n.error {
                        t.fg(Color::Red).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                )
            } else if let Some(m) = &app.control_lost {
                (
                    format!("host connection lost ({m}); actions are unavailable. q quits."),
                    t.fg(Color::Red),
                )
            } else if let Some(m) = &app.stream_issue {
                (m.clone(), t.fg(Color::Yellow))
            } else if let Some(wn) = app.storage_warnings.first().or(app.config_warnings.first()) {
                (
                    format!("warning [{}]: {}", wn.code, wn.message),
                    t.fg(Color::Yellow),
                )
            } else if app.adhoc_runs > 0 {
                (
                    format!("{} ad-hoc run(s) active (see lyra status)", app.adhoc_runs),
                    t.dim(),
                )
            } else {
                (String::new(), Style::default())
            }
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled(slice_cells(&display(&text), 0, w), style)),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let w = area.width as usize;
    let all: Vec<_> = app.bindings().into_iter().filter(|b| b.footer).collect();
    // Two spaces separate entries.
    let width = |b: &crate::app::Binding| cells(b.keys) + 1 + cells(&b.label) + 2;
    // Focus, help, and quit always stay visible; `?` lists whatever does not fit.
    let (tail, head): (Vec<_>, Vec<_>) = all
        .into_iter()
        .partition(|b| matches!(b.cmd, Cmd::Focus | Cmd::Help | Cmd::Quit));
    let mut used: usize = tail.iter().map(width).sum::<usize>().saturating_sub(2);
    let mut shown = Vec::new();
    for b in head {
        let n = width(&b);
        if used + n > w {
            continue;
        }
        used += n;
        shown.push(b);
    }
    shown.extend(tail);
    let mut spans = Vec::new();
    for (i, b) in shown.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(b.keys, t.bold()));
        spans.push(Span::raw(format!(" {}", b.label)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let w = area.width.saturating_sub(8).min(84);
    let h = area.height.saturating_sub(4);
    let rect = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + 2,
        width: w,
        height: h,
    };
    let saved_modal_bindings = {
        // Help lists the keys of the normal mode for the current focus.
        let mut lines = vec![
            Line::from(Span::styled("Keys that work here", t.bold())),
            Line::from(""),
        ];
        let normal = app.normal_bindings();
        for b in normal {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<12}", b.keys), t.bold()),
                Span::raw(b.label),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(if app.mouse {
            "Mouse mode is on (m): the wheel scrolls; terminal selection needs Option/Shift."
        } else {
            "Mouse mode is off (m turns it on): your terminal's own text selection works."
        }));
        lines.push(Line::from(
            ": runs one public lyra command (not a shell). Forms: Tab moves, Enter runs.",
        ));
        lines.push(Line::from(
            "q and Ctrl-C close this TUI; the host stops owned work only when no controller or",
        ));
        lines.push(Line::from(
            "background lease remains. b keeps work running for 2h (lyra down stops it).",
        ));
        lines.push(Line::from(
            "If a crash leaves the terminal in raw mode, type `reset` and press Enter.",
        ));
        lines
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(saved_modal_bindings).block(Block::bordered().title(" Help · Esc closes ")),
        rect,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(1);
    let h = h.min(area.height.saturating_sub(2)).max(1);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn draw_view(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    let Some(v) = app.selected_view() else {
        return;
    };
    let w = area.width as usize;
    let r = v.view_ref.clone();
    let title = display(&v.title);
    let kind = kind_word(v.kind);
    let description = display(&v.description);
    let offset = app.offset;
    let focus = app.focus == Focus::Logs;
    let clock = |ts: Timestamp| {
        let nanos = i128::from(ts.unix_ms()) * 1_000_000;
        time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .map(|d| {
                let d = d.to_offset(offset);
                format!("{:02}:{:02}:{:02}", d.hour(), d.minute(), d.second())
            })
            .unwrap_or_else(|_| "--:--:--".into())
    };
    let Some(p) = app.view_panes.get_mut(&r) else {
        f.render_widget(Paragraph::new("loading..."), area);
        return;
    };
    let [head, bar, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(area);
    let l1 = Line::from(vec![
        Span::styled(slice_cells(&title, 0, w / 2), t.bold()),
        Span::raw(format!("  {r}  {kind} view")),
    ]);
    // Source, freshness, durability, and time are always shown; freshness never by color alone.
    let (l2, l2_style) = match &p.meta {
        None => ("reading...".to_owned(), Style::default()),
        Some(m) => match m.revision {
            None => (
                format!(
                    "no data: {} (this is not an empty result)",
                    m.freshness_reason.clone().unwrap_or_default()
                ),
                t.fg(Color::Yellow),
            ),
            Some(rev) => {
                let src = match (m.source_kind, &m.source_run_id) {
                    (_, Some(run)) => format!("from run {run}"),
                    (Some(k), None) => format!(
                        "published by {}",
                        serde_json::to_value(k)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .unwrap_or_default()
                    ),
                    (None, None) => String::new(),
                };
                let at = m.recorded_at.map_or(String::new(), |a| {
                    format!("recorded {} ({} ago)", clock(a), ago(a))
                });
                let why = match (&m.freshness, &m.freshness_reason) {
                    (lyra_protocol::view::Freshness::Current, _) | (_, None) => String::new(),
                    (_, Some(reason)) => format!(": {reason}"),
                };
                let style = match m.freshness {
                    lyra_protocol::view::Freshness::Stale => t.fg(Color::Yellow),
                    _ => Style::default(),
                };
                (
                    format!(
                        "rev {rev} · {}{why} · {at} · {src} · {}",
                        freshness_word(m.freshness),
                        durability_word(m.durability)
                    ),
                    style,
                )
            }
        },
    };
    f.render_widget(
        Paragraph::new(vec![
            l1,
            Line::from(Span::styled(slice_cells(&display(&l2), 0, w), l2_style)),
            Line::from(Span::styled(slice_cells(&description, 0, w), t.dim())),
        ]),
        head,
    );
    let mut bar_text = format!(
        "VIEW · {} {}",
        p.len(),
        match p.kind {
            lyra_protocol::manifest::ViewKind::Table => "rows",
            lyra_protocol::manifest::ViewKind::Log => "items",
            lyra_protocol::manifest::ViewKind::Tree => "shown nodes",
            _ => "lines",
        }
    );
    if p.len() > 0 {
        bar_text.push_str(&format!(" · at {}", p.cursor + 1));
    }
    if let (Some(sel), Some(cur)) = (p.sel_rev, p.revision())
        && sel != cur
    {
        bar_text.push_str(&format!(" · row chosen in rev {sel}"));
    }
    if !p.row_actions.is_empty() {
        let names: Vec<String> = p.row_actions.iter().map(ToString::to_string).collect();
        bar_text.push_str(&format!(" · row actions: {}", names.join(", ")));
    }
    if p.wrap {
        bar_text.push_str(" · wrap");
    } else if p.hscroll > 0 && p.kind != lyra_protocol::manifest::ViewKind::Table {
        bar_text.push_str(&format!(" · col +{}", p.hscroll));
    }
    if p.truncated {
        bar_text.push_str(" · partial: the host sent only part of this view");
    }
    if p.loading {
        bar_text.push_str(" · reading...");
    }
    if let Some(n) = &p.note {
        bar_text.push_str(&format!(" · {n}"));
    }
    let bar_style = if focus {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::UNDERLINED)
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("{:<w$}", slice_cells(&display(&bar_text), 0, w), w = w),
            bar_style,
        )),
        bar,
    );
    p.height = body.height as usize;
    p.width = w;
    if let Some(e) = &p.error {
        f.render_widget(
            Paragraph::new(format!("Cannot read the view: {e}"))
                .wrap(ratatui::widgets::Wrap { trim: true }),
            body,
        );
        return;
    }
    match &p.body {
        crate::views::Body::Reference(summary) => {
            f.render_widget(
                Paragraph::new(format!(
                    "{summary}\nUse `: view {r} --max-bytes 262144` for the full value."
                ))
                .wrap(ratatui::widgets::Wrap { trim: true }),
                body,
            );
            return;
        }
        crate::views::Body::Empty => {
            let msg = if p.meta.is_none() {
                "reading...".to_owned()
            } else {
                "No data is recorded for this view. That is different from an empty result."
                    .to_owned()
            };
            f.render_widget(Paragraph::new(msg), body);
            return;
        }
        crate::views::Body::Data(_) => {}
    }
    if p.len() == 0 {
        f.render_widget(Paragraph::new("(the view is empty)"), body);
        return;
    }
    let lines = p.lines(focus);
    f.render_widget(Paragraph::new(lines), body);
}

fn draw_form(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Modal::Form(form) = &app.modal else {
        return;
    };
    let rect = centered(area, 90, area.height.saturating_sub(2));
    let inner_w = rect.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut focus_line = 0usize;
    for (i, fl) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        if focused {
            focus_line = lines.len();
        }
        let label = format!(
            "{}{}{}",
            if focused { "> " } else { "  " },
            display(&fl.title),
            if fl.required { " *" } else { "" }
        );
        let mut value = fl.shown();
        if focused && !matches!(fl.kind, Kind::Boolean | Kind::Enum(_)) {
            value.push('_');
        }
        let label_w = 22.min(inner_w / 3);
        let value_w = inner_w.saturating_sub(label_w + 1);
        // Keep the end of long values (the typing position) visible.
        let vw = cells(&value);
        let shown = if vw > value_w {
            slice_cells(&value, vw - value_w, value_w)
        } else {
            value
        };
        let st = if focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<label_w$}", slice_cells(&label, 0, label_w)),
                t.bold(),
            ),
            Span::raw(" "),
            Span::styled(shown, st),
        ]));
        let mut hint = fl.hint();
        if !fl.description.is_empty() {
            hint = if hint.is_empty() {
                display(&fl.description)
            } else {
                format!("{} · {hint}", display(&fl.description))
            };
        }
        if !hint.is_empty() {
            lines.push(Line::from(Span::styled(
                slice_cells(&format!("    {hint}"), 0, inner_w),
                t.dim(),
            )));
        }
        if let Some(e) = &fl.error {
            lines.push(Line::from(Span::styled(
                slice_cells(&display(&format!("    ! {e}")), 0, inner_w),
                t.fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
        }
    }
    let mut foot = vec![Line::from("")];
    if let Some(e) = &form.error {
        foot.push(Line::from(Span::styled(
            slice_cells(&display(e), 0, inner_w),
            t.fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }
    foot.push(Line::from(Span::styled(
        slice_cells(
            if form.pending {
                "sending to the host..."
            } else {
                "* required · empty fields use the declared default · the host validates again"
            },
            0,
            inner_w,
        ),
        t.dim(),
    )));
    let body_h = (rect.height as usize).saturating_sub(2 + foot.len());
    let skip = focus_line.saturating_sub(body_h.saturating_sub(3));
    let mut shown: Vec<Line> = lines.into_iter().skip(skip).take(body_h).collect();
    while shown.len() < body_h {
        shown.push(Line::from(""));
    }
    shown.extend(foot);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(shown).block(Block::bordered().title(format!(
            " {} {} ",
            match form.intent {
                Intent::Restart => "Run again",
                _ => "Run",
            },
            form.action_ref
        ))),
        rect,
    );
}

fn draw_output(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Modal::Output(o) = &app.modal else {
        return;
    };
    let rect = centered(area, 100, area.height.saturating_sub(2));
    let w = rect.width.saturating_sub(2) as usize;
    let h = rect.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = o
        .lines
        .iter()
        .skip(o.top)
        .take(h)
        .map(|l| Line::from(slice_cells(&display(l), 0, w)))
        .collect();
    f.render_widget(Clear, rect);
    let title_style = if o.failed {
        t.fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        t.bold()
    };
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(Span::styled(
            format!(" {} ", display(&o.title)),
            title_style,
        ))),
        rect,
    );
}

fn draw_row_actions(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Modal::RowAction {
        choices,
        index,
        view_ref,
    } = &app.modal
    else {
        return;
    };
    let rect = centered(area, 50, choices.len() as u16 + 4);
    let mut lines = vec![Line::from(Span::styled(
        "Run for the selected row:",
        t.bold(),
    ))];
    for (i, c) in choices.iter().enumerate() {
        let st = if i == *index {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("  {}.{c}", view_ref.plugin),
            st,
        )));
    }
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(Block::bordered()), rect);
}

/// The run's freshness word (§14.6); runs without provenance read as historical.
fn freshness_of(rec: &lyra_protocol::run::RunRecord) -> &'static str {
    freshness_word(
        rec.provenance
            .as_ref()
            .map_or(lyra_protocol::view::Freshness::Historical, |p| p.freshness),
    )
}

fn run_word(l: Lifecycle) -> &'static str {
    match l {
        Lifecycle::Starting => "starting",
        Lifecycle::Running => "running",
        Lifecycle::Stopping { .. } => "stopping",
        Lifecycle::Finished { outcome } => match outcome {
            Outcome::Succeeded => "succeeded",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "stopped",
            Outcome::TimedOut => "timed out",
            Outcome::Interrupted => "interrupted",
        },
    }
}

fn took(ms: i64) -> String {
    let s = ms / 1000;
    match ms {
        ..1000 => format!("{}ms", ms.max(0)),
        1000..60_000 => format!("{}.{}s", s, (ms % 1000) / 100),
        60_000..3_600_000 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Start time: `HH:MM:SS` today, `MM-DD HH:MM` on other days.
fn started_word(app: &App, ts: Timestamp) -> String {
    let at = |t: Timestamp| {
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(t.unix_ms()) * 1_000_000)
            .ok()
            .map(|d| d.to_offset(app.offset))
    };
    match (at(ts), at(Timestamp::now())) {
        (Some(d), Some(now)) if d.date() != now.date() => format!(
            "{:02}-{:02} {:02}:{:02}",
            u8::from(d.month()),
            d.day(),
            d.hour(),
            d.minute()
        ),
        _ => app.clock(ts, true),
    }
}

fn draw_history(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Modal::History {
        action_ref,
        runs,
        error,
        index,
    } = &app.modal
    else {
        return;
    };
    let n = runs.as_ref().map_or(1, |r| r.len().max(1));
    let rect = centered(area, 72, n as u16 + 4);
    let w = rect.width.saturating_sub(2) as usize;
    let shown = app.panes.get(action_ref).and_then(|p| p.run_id.as_ref());
    let mut lines = vec![Line::from(Span::styled(
        slice_cells(
            &format!(
                "{:<12}{:<13}{:<9}{:<12}{}",
                "started", "outcome", "took", "run", "state"
            ),
            0,
            w,
        ),
        t.dim(),
    ))];
    match (runs, error) {
        (_, Some(e)) => lines.push(Line::from(Span::styled(
            slice_cells(&display(&format!("Cannot read run history: {e}")), 0, w),
            t.fg(Color::Red),
        ))),
        (None, None) => lines.push(Line::from("reading...")),
        (Some(r), None) if r.is_empty() => lines.push(Line::from("No runs recorded yet.")),
        (Some(r), None) => {
            let body_h = (rect.height as usize).saturating_sub(3).max(1);
            let skip = (index + 1).saturating_sub(body_h);
            for (i, rec) in r.iter().enumerate().skip(skip).take(body_h) {
                let mut outcome = run_word(rec.lifecycle).to_owned();
                if let Some(c) = rec.exit.as_ref().and_then(|e| e.code)
                    && c != 0
                {
                    outcome = format!("{outcome} ({c})");
                }
                let dur = match rec.ended_at {
                    Some(e) => took(e.unix_ms() - rec.started_at.unix_ms()),
                    None => format!("{} so far", ago(rec.started_at)),
                };
                let id = rec.run_id.to_string();
                let mut state = if rec.lifecycle.is_active() {
                    "current".to_owned()
                } else {
                    freshness_of(rec).to_owned()
                };
                if Some(&rec.run_id) == shown {
                    state.push_str(" · shown");
                }
                let text = format!(
                    "{:<12}{:<13}{:<9}{:<12}{state}",
                    started_word(app, rec.started_at),
                    outcome,
                    dur,
                    slice_cells(&id, 0, 10),
                );
                let st = if i == *index {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                let color = match rec.lifecycle {
                    Lifecycle::Finished {
                        outcome: Outcome::Succeeded,
                    } => Style::default(),
                    Lifecycle::Finished {
                        outcome: Outcome::Cancelled,
                    } => t.dim(),
                    Lifecycle::Finished { .. } => t.fg(Color::Red),
                    _ => t.fg(Color::Green),
                };
                lines.push(Line::from(Span::styled(
                    format!("{:<w$}", slice_cells(&text, 0, w), w = w),
                    st.patch(color),
                )));
            }
        }
    }
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(
            " History · {} ",
            slice_cells(&action_ref.to_string(), 0, w.saturating_sub(12))
        ))),
        rect,
    );
}
