//! `lyra` with no command: the human TUI (§12). Agents get `TTY_REQUIRED` at once instead
//! of a blocked process; an unconfigured workspace gets one screen of setup instructions.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::reply::ReplyContext;
use lyra_tui::TuiEnd;

use super::ctx::Ctx;

pub fn open(ctx: &Ctx) -> ExitCode {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return ctx.fail(
            ReplyContext::default(),
            ErrorInfo::new(
                ErrorCode::TTY_REQUIRED,
                "`lyra` without a command opens the TUI and needs an interactive terminal",
            )
            .with_next_action(
                &["lyra", "status", "--json"],
                "Agents use the structured CLI; `lyra --help` lists every command.",
            ),
        );
    }
    let paths = match ctx.paths() {
        Ok(p) => p,
        Err(e) => return ctx.fail(ReplyContext::default(), e),
    };
    let root = paths.root.to_string();
    match lyra_tui::run(paths) {
        Ok(TuiEnd::Closed(message)) => {
            // The window may already be gone (SIGHUP); nothing else to report then.
            let _ = writeln!(std::io::stderr(), "lyra: {message}");
            ExitCode::SUCCESS
        }
        Ok(TuiEnd::NotSetup(e)) => {
            let _ = std::io::stdout().write_all(setup_screen(&root, &e).as_bytes());
            ExitCode::from(e.code.exit_code())
        }
        Err(e) => ctx.fail(ReplyContext::default(), e),
    }
}

fn setup_screen(root: &str, e: &ErrorInfo) -> String {
    format!(
        "Lyra is not set up in {root}
{message}.

Set it up in one step. Lyra never guesses commands; your coding agent writes the tools:

  lyra setup

It installs the Lyra skills for your agent and prints the one command to run
(for example `claude '...'`). When the agent is done, run `lyra` again.

Agents: run `lyra setup --json` for read-only project facts; see `lyra --help`.
",
        message = e.message
    )
}
