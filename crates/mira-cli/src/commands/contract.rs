//! Commands that need no host: `schema` and `validate`.

use std::path::Path;
use std::process::ExitCode;

use mira_protocol::config::{Draft, validate_draft};
use mira_protocol::reply::{PublicReply, ReplyContext, ReplyMeta};
use mira_protocol::{ErrorCode, ErrorInfo, schemas};
use serde::Serialize;

use crate::output::{self, Mode};

/// `mira schema NAME`: the raw JSON Schema, not wrapped in a reply.
pub fn schema(mode: Mode, name: &str) -> ExitCode {
    match schemas::schema(name) {
        Some(v) => match serde_json::to_string_pretty(&v) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => output::fail(
                mode,
                ReplyContext::default(),
                ErrorInfo::new(ErrorCode::INTERNAL, e.to_string()),
            ),
        },
        None => output::fail(
            mode,
            ReplyContext::default(),
            ErrorInfo::new(
                ErrorCode::NOT_FOUND,
                format!(
                    "unknown schema `{name}`; expected one of: {}",
                    schemas::NAMES.join(", ")
                ),
            ),
        ),
    }
}

#[derive(Serialize)]
struct PluginReport {
    id: String,
    dir: String,
    enabled: bool,
    actions: Vec<String>,
    views: Vec<String>,
}

#[derive(Serialize)]
struct ValidateReport {
    kind: &'static str,
    path: String,
    workspace_name: Option<String>,
    plugins: Vec<PluginReport>,
    set_hash: Option<String>,
}

fn report(p: &mira_protocol::config::LoadedPlugin) -> PluginReport {
    PluginReport {
        id: p.plugin.id.to_string(),
        dir: p.dir.to_string(),
        enabled: p.plugin.enabled,
        actions: p.plugin.actions.iter().map(|a| a.id.to_string()).collect(),
        views: p.plugin.views.iter().map(|v| v.id.to_string()).collect(),
    }
}

/// `mira validate PATH`: pure validation; never executes plugin or project code.
pub fn validate(mode: Mode, path: &Path) -> ExitCode {
    let shown = path.to_string_lossy().into_owned();
    if std::fs::symlink_metadata(path).is_err() {
        return output::fail(
            mode,
            ReplyContext::default(),
            ErrorInfo::new(ErrorCode::NOT_FOUND, format!("`{shown}` does not exist.")),
        );
    }
    let reply = match validate_draft(path) {
        Ok(Draft::Workspace(set)) => PublicReply::success(
            ReplyContext::default(),
            ValidateReport {
                kind: "workspace",
                path: shown,
                workspace_name: Some(set.workspace.name.clone()),
                plugins: set.plugins.iter().map(report).collect(),
                set_hash: Some(set.set_hash.to_string()),
            },
            ReplyMeta::default(),
        ),
        Ok(Draft::Plugin(p)) => PublicReply::success(
            ReplyContext::default(),
            ValidateReport {
                kind: "plugin",
                path: shown,
                workspace_name: None,
                plugins: vec![report(&p)],
                set_hash: None,
            },
            ReplyMeta::default(),
        ),
        Err(issues) => PublicReply::failure(ReplyContext::default(), issues.to_error_info()),
    };
    output::emit(mode, &reply, |r| {
        let kind = if r.kind == "workspace" {
            "project"
        } else {
            r.kind
        };
        let mut s = format!("valid {kind} at {}", r.path);
        for p in &r.plugins {
            s.push_str(&format!(
                "\n  {} ({}): {} action(s), {} view(s){}",
                p.id,
                p.dir,
                p.actions.len(),
                p.views.len(),
                if p.enabled { "" } else { ", disabled" }
            ));
        }
        s
    })
}
