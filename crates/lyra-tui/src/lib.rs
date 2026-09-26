//! The human TUI (§12): a projection of host state over the shared typed client.
//!
//! The TUI never writes run state, never spawns project commands, and never blocks on IPC.
//! It joins the workspace session as a controller; closing it (q, Ctrl-C, SIGHUP) restores
//! the terminal at once and lets the host decide, by its session rules, whether owned work
//! stops.

mod app;
mod clip;
mod cmdbar;
mod form;
mod git;
mod ipc;
mod logs;
mod term;
mod terminal;
mod ui;
mod views;

use std::time::{Duration, Instant};

use lyra_client::connect;
use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::ipc::*;
use lyra_protocol::paths::WorkspacePaths;
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{App, Io, Quit};
use crate::ipc::{Event, Failure, Read, Tx};

/// Redraws caused by stream events are merged to at most ~30 frames per second.
const FRAME_GAP: Duration = Duration::from_millis(33);
/// Coarse tick for clocks and countdowns when nothing else happens.
const TICK: Duration = Duration::from_secs(1);
/// Events handled before a redraw gets a chance.
const EVENT_BATCH: usize = 4096;

pub enum TuiEnd {
    /// The workspace has no configuration; nothing was opened.
    NotSetup(ErrorInfo),
    /// The TUI closed; the line says what happens to the session's work.
    Closed(String),
}

/// Opens the TUI for a workspace. The caller has checked that stdin and stdout are TTYs.
pub fn run(paths: WorkspacePaths) -> Result<TuiEnd, ErrorInfo> {
    // Read the local offset while the process is still single-threaded.
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| ErrorInfo::new(ErrorCode::INTERNAL, format!("cannot start runtime: {e}")))?;
    let res = rt.block_on(serve(paths, offset));
    // Do not wait for workers or the input thread: the host owns shutdown of the work.
    rt.shutdown_background();
    res
}

async fn serve(paths: WorkspacePaths, offset: time::UtcOffset) -> Result<TuiEnd, ErrorInfo> {
    let mut control = connect(&paths, &ipc::options(ConnectionKind::Control))
        .await
        .map_err(|e| e.to_error_info())?;
    let (catalog, catalog_revision) = match ipc::call::<_, CatalogList>(
        &mut control,
        Method::CatalogListM,
        &ipc::catalog_params(),
    )
    .await
    {
        Ok(a) => (Ok(a.data), a.catalog_revision),
        Err(Failure::Reply(e)) if e.code == ErrorCode::NOT_SETUP => {
            return Ok(TuiEnd::NotSetup(e));
        }
        Err(Failure::Lost(m)) => return Err(ErrorInfo::new(ErrorCode::INTERNAL, m)),
        Err(f) => (Err(f.info()), None),
    };
    let env = ClientEnv::capture().map_err(|m| ErrorInfo::new(ErrorCode::INVALID_ARGUMENT, m))?;
    let attached = ipc::call::<_, SessionData>(
        &mut control,
        Method::SessionAttach,
        &ipc::attach_params(&env),
    )
    .await;
    let reader = connect(&paths, &ipc::options(ConnectionKind::Control))
        .await
        .map_err(|e| e.to_error_info())?;
    let stream = connect(&paths, &ipc::options(ConnectionKind::Stream))
        .await
        .ok();

    let (tx, mut rx) = unbounded_channel::<Event>();
    let (control_tx, control_rx) = unbounded_channel();
    let (read_tx, read_rx) = unbounded_channel();
    tokio::spawn(ipc::control_worker(control, env, control_rx, tx.clone()));
    tokio::spawn(ipc::read_worker(
        reader,
        catalog_revision,
        read_rx,
        tx.clone(),
    ));
    tokio::spawn(ipc::stream_worker(paths.clone(), stream, tx.clone()));
    tokio::spawn(signals(tx.clone()));

    let mut app = App::new(
        paths.root.to_string(),
        offset,
        Io {
            control: control_tx,
            read: read_tx.clone(),
            events: tx.clone(),
            paths: paths.clone(),
        },
    );
    match attached {
        Ok(a) => app.set_attached(a.data.session),
        Err(f) => {
            let e = f.info();
            app.error(format!(
                "could not join the session: [{}] {}",
                e.code, e.message
            ));
        }
    }
    match catalog {
        Ok(c) => app.set_catalog(c),
        Err(e) => app.catalog_error = Some(e),
    }
    let _ = read_tx.send(Read::Recent);

    let mut guard = term::TerminalGuard::enter().map_err(|e| {
        ErrorInfo::new(
            ErrorCode::INTERNAL,
            format!("cannot open the terminal: {e}"),
        )
    })?;
    spawn_input(tx.clone());

    let color = std::env::var_os("NO_COLOR").is_none();
    let mut last_draw = Instant::now() - FRAME_GAP;
    let mut dirty = true;
    let mut urgent = true;
    loop {
        if dirty && (urgent || last_draw.elapsed() >= FRAME_GAP) {
            if guard
                .terminal
                .draw(|f| ui::draw(f, &mut app, color))
                .is_err()
            {
                app.quit
                    .get_or_insert(Quit::Normal(Some("terminal closed")));
            }
            last_draw = Instant::now();
            dirty = false;
            urgent = false;
        }
        if app.quit.is_some() {
            break;
        }
        let wait = if dirty {
            FRAME_GAP.saturating_sub(last_draw.elapsed())
        } else {
            TICK
        };
        tokio::select! {
            ev = rx.recv() => {
                let mut next = ev;
                let mut n = 0usize;
                while let Some(ev) = next {
                    // Input is drawn at the next chance; stream updates are merged.
                    if matches!(ev, Event::Input(_) | Event::Signal(_)) {
                        urgent = true;
                    }
                    app.handle(ev);
                    n += 1;
                    if n >= EVENT_BATCH || app.quit.is_some() {
                        break;
                    }
                    next = rx.try_recv().ok();
                }
                dirty = true;
                if let Some(on) = app.mouse_changed.take() {
                    term::set_mouse(on);
                }
            }
            _ = tokio::time::sleep(wait) => {
                app.tick();
                dirty = true;
            }
        }
    }
    let message = match app.quit.take() {
        Some(Quit::Kept(until)) => format!(
            "Lyra keeps this session running in the background{}. `lyra down` stops it; `lyra` reopens it.",
            until
                .map(|t| format!(" until {}", app.clock(t, false)))
                .unwrap_or_default()
        ),
        Some(Quit::Normal(Some(sig))) => format!("{sig}: {}", app.quit_effect(false)),
        Some(Quit::Normal(None)) | None => app.quit_effect(false),
    };
    // Restore the outer terminal now; the host finishes any stop on its own.
    drop(guard);
    Ok(TuiEnd::Closed(message))
}

/// Terminal input on a plain thread; it ends with the process.
fn spawn_input(tx: Tx) {
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if tx.send(Event::Input(ev)).is_err() {
                break;
            }
        }
    });
}

/// Window close (SIGHUP) and TERM/INT follow the same close rules as `q`.
async fn signals(tx: Tx) {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut hup), Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::hangup()),
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return;
    };
    let name = tokio::select! {
        _ = hup.recv() => "SIGHUP",
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
    };
    let _ = tx.send(Event::Signal(name));
}
