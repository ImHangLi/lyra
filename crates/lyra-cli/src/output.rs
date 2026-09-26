//! Output rules (§10.2): with `--json` (default when stdout is not a TTY) stdout holds exactly
//! one JSON object plus LF. Text output is a rendering of the same typed data.

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use lyra_protocol::reply::{PublicReply, ReplyContext};
use lyra_protocol::{ErrorCode, ErrorInfo};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Json,
    Text,
}

impl Mode {
    pub fn resolve(json: bool, text: bool) -> Self {
        match (json, text) {
            (true, _) => Self::Json,
            (_, true) => Self::Text,
            _ if std::io::stdout().is_terminal() => Self::Text,
            _ => Self::Json,
        }
    }
}

/// Prints a reply and returns its fixed exit code.
pub fn emit<T: Serialize>(
    mode: Mode,
    reply: &PublicReply<T>,
    text: impl FnOnce(&T) -> String,
) -> ExitCode {
    let mut out = std::io::stdout().lock();
    let written = match mode {
        Mode::Json => match serde_json::to_string(reply) {
            Ok(line) => writeln!(out, "{line}"),
            Err(e) => writeln!(
                out,
                "{{\"api\":1,\"ok\":false,\"error\":{{\"code\":\"INTERNAL\",\"message\":{:?},\"retryable\":false}}}}",
                e.to_string()
            ),
        },
        Mode::Text => match (reply.data(), reply.error()) {
            (_, Some(e)) => {
                let _ = writeln!(std::io::stderr(), "{}", render_error(e));
                Ok(())
            }
            (Some(d), None) => writeln!(out, "{}", text(d)),
            (None, None) => Ok(()),
        },
    };
    if written.is_err() {
        return ExitCode::from(7);
    }
    ExitCode::from(reply.exit_code())
}

pub fn fail(mode: Mode, ctx: ReplyContext, error: ErrorInfo) -> ExitCode {
    emit::<()>(mode, &PublicReply::failure(ctx, error), |_| String::new())
}

pub fn render_error(e: &ErrorInfo) -> String {
    let mut s = format!("error[{}]: {}", e.code, e.message);
    if let Some(details) = &e.details
        && let Some(issues) = details.get("issues").and_then(|v| v.as_array())
    {
        for i in issues.iter().skip(1) {
            let file = i.get("file").and_then(|v| v.as_str()).unwrap_or("");
            let ptr = i.get("pointer").and_then(|v| v.as_str()).unwrap_or("");
            let msg = i.get("message").and_then(|v| v.as_str()).unwrap_or("");
            s.push_str(&format!("\n  {file} `{ptr}`: {msg}"));
        }
    }
    if let Some(n) = &e.next_action {
        s.push_str(&format!("\n  next: {} ({})", n.argv.join(" "), n.reason));
    }
    s
}

pub fn invalid_argument(message: impl Into<String>) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::INVALID_ARGUMENT, message)
}
