//! `lyra terminal` and `lyra input` (§10.2, §13): agents read and type into a shared PTY.
//!
//! Typical agent loop: `lyra up --background`, `lyra run ACTION --no-wait` for a PTY action,
//! then `lyra terminal RUN` to read the screen and `lyra input RUN --text ...` /
//! `--key enter` to answer. Each `input` call takes the input lock only for that write.

use std::process::ExitCode;

use lyra_client::ConnectOptions;
use lyra_protocol::ids::{RunId, ScreenRevision};
use lyra_protocol::ipc::*;
use lyra_protocol::reply::{PublicReply, ReplyContext};

use super::ctx::{Ctx, block_on};
use crate::output::invalid_argument;

fn parse_run(s: &str) -> Result<RunId, lyra_protocol::ErrorInfo> {
    RunId::parse(s.to_owned()).map_err(|e| invalid_argument(format!("{e}: `{s}`")))
}

/// Text form: one status line, then the screen rows without trailing blank rows.
pub fn screen_text(s: &TerminalSnapshot) -> String {
    let mut flags = Vec::new();
    if s.alternate_screen {
        flags.push("alternate screen".to_owned());
    }
    if s.exited {
        flags.push("exited".to_owned());
    }
    if let Some(o) = &s.input_owner {
        flags.push(format!("input held by {o}"));
    }
    let mut out = format!(
        "{}  screen {}  {}x{}  cursor {},{}{}",
        s.run_id,
        s.screen_revision,
        s.cols,
        s.rows,
        s.cursor.row,
        s.cursor.col,
        if flags.is_empty() {
            String::new()
        } else {
            format!("  ({})", flags.join(", "))
        }
    );
    let used = s
        .lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    for line in &s.lines[..used] {
        out.push('\n');
        out.push_str(line);
    }
    out
}

pub fn terminal(ctx: &Ctx, run: &str, max_bytes: Option<u32>) -> ExitCode {
    block_on(async {
        let run_id = match parse_run(run) {
            Ok(r) => r,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match ctx.client(&ConnectOptions::cli()).await {
            Ok(c) => c,
            Err((c, e)) => return ctx.fail(c, e),
        };
        let p = TerminalSnapshotParams {
            run_id,
            row_start: None,
            row_count: None,
            include_style: false,
            max_bytes,
        };
        let reply: PublicReply<TerminalSnapshot> =
            match client.call(Method::TerminalSnapshotM, &p).await {
                Ok(r) => r,
                Err(e) => return ctx.fail(client.context(), e.to_error_info()),
            };
        ctx.emit(&reply, screen_text)
    })
}

pub fn input(
    ctx: &Ctx,
    run: &str,
    text: Option<String>,
    key: Option<String>,
    expected_screen_revision: Option<u64>,
) -> ExitCode {
    block_on(async {
        let prepared = (|| {
            let run_id = parse_run(run)?;
            let input = match (text, key) {
                (Some(text), None) => TerminalInput::Text { text },
                (None, Some(key)) => TerminalInput::Key { key },
                _ => return Err(invalid_argument("use exactly one of --text or --key")),
            };
            let expected = expected_screen_revision
                .map(ScreenRevision::new)
                .transpose()
                .map_err(|e| invalid_argument(e.to_string()))?;
            Ok((run_id, input, expected))
        })();
        let (run_id, input, expected_screen_revision) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match ctx.client(&ConnectOptions::cli()).await {
            Ok(c) => c,
            Err((c, e)) => return ctx.fail(c, e),
        };
        let p = TerminalInputParams {
            run_id,
            input,
            expected_screen_revision,
        };
        let reply: PublicReply<TerminalSnapshot> =
            match client.call(Method::TerminalInputM, &p).await {
                Ok(r) => r,
                Err(e) => return ctx.fail(client.context(), e.to_error_info()),
            };
        ctx.emit(&reply, screen_text)
    })
}
