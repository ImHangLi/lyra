//! Terminal ownership (§12.6, §13.5): one guard enters raw mode and the alternate screen,
//! and every exit path (normal, error, panic) restores the outer terminal exactly once.

use std::io::{Stdout, Write};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

static ACTIVE: AtomicBool = AtomicBool::new(false);

pub type Term = Terminal<CrosstermBackend<Stdout>>;

/// Restores the outer terminal. Safe to call more than once and from the panic hook.
pub fn restore() {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let mut out = std::io::stdout();
        let _ = disable_raw_mode();
        let _ = execute!(out, LeaveAlternateScreen, Show);
        let _ = out.flush();
    }
}

pub struct TerminalGuard {
    /// Never dropped: ratatui's `Drop` prints to stderr when showing the cursor fails, and
    /// that print panics once the window is gone (SIGHUP). [`restore`] does its work instead.
    pub terminal: ManuallyDrop<Term>,
}

impl TerminalGuard {
    pub fn enter() -> std::io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        let mut out = std::io::stdout();
        if let Err(e) = execute!(out, EnterAlternateScreen, Hide) {
            restore();
            return Err(e);
        }
        match Terminal::new(CrosstermBackend::new(std::io::stdout())) {
            Ok(terminal) => Ok(Self {
                terminal: ManuallyDrop::new(terminal),
            }),
            Err(e) => {
                restore();
                Err(e)
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn install_panic_hook() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore first so the panic message lands on the normal screen.
        restore();
        previous(info);
    }));
}
