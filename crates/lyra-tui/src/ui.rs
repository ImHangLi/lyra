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

use crate::app::{App, Cmd, Focus, Inputs, Intent, Item, Modal};
use crate::logs::{cells, display, slice_cells};

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
    if app.narrow {
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
    if matches!(app.modal, Modal::Help) {
        draw_help(f, app, &t, area);
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
    let root = short_root(&app.root, left_max);
    let gap = (area.width as usize).saturating_sub(6 + cells(&root) + rw);
    let line = Line::from(vec![
        Span::styled("Lyra", t.bold()),
        Span::raw("  "),
        Span::raw(root),
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
                    app.items.len()
                ),
                0,
                w,
            ),
            t.dim(),
        )));
    }
    if app.items.is_empty() {
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
    for (vi, &ii) in app.visible.iter().enumerate() {
        let item = &app.items[ii];
        let plugin = item.action_ref.plugin.to_string();
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
        let (state, style) = state_label(app, t, item);
        let sw = cells(&state);
        let marker = if selected { "> " } else { "  " };
        let title_w = w.saturating_sub(2 + sw + 1);
        let title = slice_cells(&item.title, 0, title_w);
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
        Some(Inputs::Required(f)) => {
            format!(" · needs input: {} (forms arrive in LYR-08)", f.join(", "))
        }
        Some(Inputs::Optional(f)) => format!(" · optional input: {}", f.join(", ")),
        Some(Inputs::Unknown(m)) => format!(" · inputs unknown: {m}"),
        _ => String::new(),
    };
    let l2 = format!("{state}{inputs}");
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
    let Some(p) = app.panes.get_mut(&a) else {
        f.render_widget(Paragraph::new("loading..."), body);
        return;
    };
    p.height = body.height as usize;
    p.width = text_w;
    // Status bar of the log panel.
    let mut bar_text = String::from("LOGS");
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
        Modal::Confirm {
            action_ref, fields, ..
        } => (
            format!(
                "{action_ref} takes optional input ({}); input forms arrive in LYR-08. Run with defaults?",
                fields.join(", ")
            ),
            t.bold(),
        ),
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
        lines.push(Line::from(
            "Mouse is off, so your terminal's own text selection works.",
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
