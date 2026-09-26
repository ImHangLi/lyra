//! Observe-only commands: `status`, `catalog`, `describe`, `paths`, `doctor`.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

use lyra_client::ConnectOptions;
use lyra_protocol::config::validate_draft;
use lyra_protocol::ids::{CatalogRevision, ItemRef, WorkspaceId};
use lyra_protocol::ipc::*;
use lyra_protocol::manifest::Runner;
use lyra_protocol::paths::{WorkspacePaths, current_uid};
use lyra_protocol::reply::{PublicReply, ReplyContext, ReplyMeta};
use lyra_protocol::run::Lifecycle;
use serde::Serialize;

use super::ctx::{Ctx, block_on};

/// Lower-case Debug text for simple enums.
pub fn lc<T: std::fmt::Debug>(v: &T) -> String {
    format!("{v:?}").to_lowercase()
}

pub fn lifecycle_text(l: &Lifecycle) -> String {
    match l {
        Lifecycle::Starting => "starting".into(),
        Lifecycle::Running => "running".into(),
        Lifecycle::Stopping { reason } => format!("stopping ({reason:?})").to_lowercase(),
        Lifecycle::Finished { outcome } => format!("finished: {outcome:?}").to_lowercase(),
    }
}

pub fn status(ctx: &Ctx) -> ExitCode {
    block_on(async {
        let mut client = match ctx.client(&ConnectOptions::cli()).await {
            Ok(c) => c,
            Err((c, e)) => return ctx.fail(c, e),
        };
        match client.status().await {
            Ok(reply) => ctx.emit(&reply, |s| {
                let mut out = match &s.session {
                    None => "session: none".to_owned(),
                    Some(se) => format!(
                        "session: {} {}, {} controller(s){}",
                        lc(&se.mode),
                        lc(&se.state),
                        se.controller_count,
                        se.expires_at
                            .map(|t| format!(", expires {t}"))
                            .unwrap_or_default()
                    ),
                };
                if s.runs.is_empty() {
                    out.push_str("\nruns: none active");
                }
                for r in &s.runs {
                    let target = r
                        .action_ref
                        .as_ref()
                        .map_or("exec".to_owned(), ToString::to_string);
                    out.push_str(&format!(
                        "\n  {}  {:<24} {}",
                        r.run_id,
                        target,
                        lifecycle_text(&r.lifecycle)
                    ));
                }
                for sc in &s.schedules {
                    out.push_str(&format!("\nschedule: {}", super::schedule::text(sc)));
                }
                for w in s.storage_warnings.iter().chain(&s.config_warnings) {
                    out.push_str(&format!("\nwarning[{}]: {}", w.code, w.message));
                }
                out
            }),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub struct CatalogArgs {
    pub search: Option<String>,
    pub if_revision: Option<u64>,
    pub if_workspace: Option<String>,
    pub limit: Option<u32>,
    pub after: Option<String>,
    pub max_bytes: Option<u32>,
}

pub fn catalog(ctx: &Ctx, args: CatalogArgs) -> ExitCode {
    let CatalogArgs {
        search,
        if_revision,
        if_workspace,
        limit,
        after,
        max_bytes,
    } = args;
    block_on(async {
        let parsed = (|| {
            let rev = if_revision
                .map(CatalogRevision::new)
                .transpose()
                .map_err(|e| e.to_string())?;
            let ws = if_workspace
                .map(WorkspaceId::parse)
                .transpose()
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((rev, ws))
        })();
        let (if_revision, if_workspace) = match parsed {
            Ok(v) => v,
            Err(e) => {
                return ctx.fail(ReplyContext::default(), crate::output::invalid_argument(e));
            }
        };
        let mut client = match ctx.client(&ConnectOptions::cli()).await {
            Ok(c) => c,
            Err((c, e)) => return ctx.fail(c, e),
        };
        let p = CatalogListParams {
            query: search,
            if_revision,
            if_workspace,
            cursor: after,
            limit,
            max_bytes,
        };
        match client
            .call::<_, CatalogList>(Method::CatalogListM, &p)
            .await
        {
            Ok(reply) => {
                let not_modified = reply.meta().not_modified;
                ctx.emit(&reply, |c| {
                    if not_modified {
                        return "catalog not modified".to_owned();
                    }
                    if c.items.is_empty() {
                        return "no matching tools".to_owned();
                    }
                    c.items
                        .iter()
                        .map(|i| {
                            let kind = match &i.item {
                                CatalogItemKind::Action { mode } => {
                                    format!("{mode:?}").to_lowercase()
                                }
                                CatalogItemKind::View { view_kind } => {
                                    format!("view:{view_kind:?}").to_lowercase()
                                }
                            };
                            format!(
                                "{:<28} {:<10} {}{}",
                                i.item_ref.to_string(),
                                kind,
                                i.title,
                                if i.enabled { "" } else { " (disabled)" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn describe(ctx: &Ctx, item: String, include_schema: bool, max_bytes: Option<u32>) -> ExitCode {
    block_on(async {
        let item_ref: ItemRef = match item.parse() {
            Ok(r) => r,
            Err(e) => {
                return ctx.fail(
                    ReplyContext::default(),
                    crate::output::invalid_argument(format!("{e}: `{item}`")),
                );
            }
        };
        let mut client = match ctx.client(&ConnectOptions::cli()).await {
            Ok(c) => c,
            Err((c, e)) => return ctx.fail(c, e),
        };
        match client
            .call::<_, ItemDescription>(
                Method::ItemDescribe,
                &ItemDescribeParams {
                    item_ref,
                    include_schema,
                    max_bytes,
                },
            )
            .await
        {
            Ok(reply) => ctx.emit(&reply, |d| {
                let mut s = format!(
                    "{}  {}\n  {}\n  plugin: {} ({})",
                    d.item.item_ref, d.item.title, d.item.description, d.plugin.name, d.plugin.id
                );
                if let Some(a) = &d.action {
                    s.push_str(
                        &format!(
                            "\n  {:?} via {} runner, terminal {:?}, cwd {}",
                            a.mode, a.runner, a.terminal, a.cwd
                        )
                        .to_lowercase(),
                    );
                    if !a.effects.is_empty() {
                        s.push_str(&format!("\n  effects: {}", a.effects.join(", ")));
                    }
                }
                if let Some(v) = &d.view {
                    s.push_str(
                        &format!(
                            "\n  view kind {:?}, persistence {:?}",
                            v.view_kind, v.persistence
                        )
                        .to_lowercase(),
                    );
                }
                s.push_str(&format!("\n  try: {}", d.invoke_hint.join(" ")));
                s
            }),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn paths(ctx: &Ctx) -> ExitCode {
    // Path computation is deterministic, so no host is needed.
    let p = match ctx.paths() {
        Ok(p) => p,
        Err(e) => return ctx.fail(ReplyContext::default(), e),
    };
    let data = p.to_data();
    let reply = PublicReply::success(
        ReplyContext {
            workspace: Some(data.workspace.clone()),
            ..ReplyContext::default()
        },
        data,
        ReplyMeta::default(),
    );
    ctx.emit(&reply, |d| {
        d.entries
            .iter()
            .map(|e| {
                format!(
                    "{:<16} {}\n                 {}",
                    format!("{:?}", e.class).to_lowercase(),
                    e.path,
                    e.purpose
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Serialize)]
struct Check {
    name: String,
    status: CheckStatus,
    message: String,
}

#[derive(Serialize)]
struct DoctorData {
    version: &'static str,
    protocol_hash: String,
    checks: Vec<Check>,
}

fn on_path(program: &str, root: &Path) -> bool {
    if program.contains('/') {
        let p = Path::new(program);
        return if p.is_absolute() {
            p.exists()
        } else {
            root.join(p).exists()
        };
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

pub fn doctor(ctx: &Ctx) -> ExitCode {
    block_on(async {
        let mut checks = Vec::new();
        let mut push = |name: &str, status: CheckStatus, message: String| {
            checks.push(Check {
                name: name.to_owned(),
                status,
                message,
            })
        };
        let selected = match ctx.select() {
            Ok(s) => s,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        push(
            "workspace",
            CheckStatus::Ok,
            format!("{} ({})", selected.root, lc(&selected.reason)),
        );
        let paths = WorkspacePaths::new(selected.root.clone());
        let mut set = None;
        if selected.is_setup() {
            match validate_draft(&paths.lyra_dir) {
                Ok(lyra_protocol::config::Draft::Workspace(s)) => {
                    push(
                        "config",
                        CheckStatus::Ok,
                        format!("{} plugin(s) valid", s.plugins.len()),
                    );
                    set = Some(s);
                }
                Ok(_) => push("config", CheckStatus::Fail, "unexpected draft kind".into()),
                Err(i) => push("config", CheckStatus::Fail, i.to_error_info().message),
            }
        } else {
            push(
                "config",
                CheckStatus::Warn,
                "not set up: .lyra/workspace.json is missing; run `lyra setup --json`".into(),
            );
        }
        match std::fs::symlink_metadata(&paths.runtime_dir) {
            Ok(m)
                if m.is_dir()
                    && m.uid() == current_uid()
                    && m.permissions().mode() & 0o077 == 0 =>
            {
                push(
                    "runtime_dir",
                    CheckStatus::Ok,
                    paths.runtime_dir.display().to_string(),
                )
            }
            Ok(_) => push(
                "runtime_dir",
                CheckStatus::Fail,
                format!(
                    "{} must be a 0700 directory owned by you",
                    paths.runtime_dir.display()
                ),
            ),
            Err(_) => push("runtime_dir", CheckStatus::Ok, "not created yet".into()),
        }
        match lyra_client::connect(&paths, &ConnectOptions::cli()).await {
            Ok(c) => push(
                "host",
                CheckStatus::Ok,
                format!("reachable, epoch {}", c.hello().host_epoch),
            ),
            Err(e) => push("host", CheckStatus::Fail, e.to_string()),
        }
        if let Some(set) = &set {
            for lp in &set.plugins {
                let mut programs: Vec<(String, String)> = Vec::new();
                if let Some(e) = &lp.plugin.entry {
                    programs.push((format!("{} entry", lp.plugin.id), e.program().to_owned()));
                }
                for a in &lp.plugin.actions {
                    if let Runner::Command { argv } = &a.run {
                        programs.push((
                            format!("{}.{}", lp.plugin.id, a.id),
                            argv.program().to_owned(),
                        ));
                    }
                }
                for (what, program) in programs {
                    let base = if what.ends_with(" entry") {
                        lp.dir.as_path()
                    } else {
                        paths.root.as_path()
                    };
                    if !on_path(&program, base) {
                        push(
                            "executable",
                            CheckStatus::Warn,
                            format!("{what}: `{program}` was not found on PATH"),
                        );
                    }
                }
            }
        }
        let data = DoctorData {
            version: lyra_protocol::VERSION,
            protocol_hash: lyra_protocol::schemas::protocol_hash().to_string(),
            checks,
        };
        let failed = data
            .checks
            .iter()
            .any(|c| matches!(c.status, CheckStatus::Fail));
        let reply = PublicReply::success(ReplyContext::default(), data, ReplyMeta::default());
        let code = ctx.emit(&reply, |d| {
            d.checks
                .iter()
                .map(|c| {
                    let tag = match c.status {
                        CheckStatus::Ok => "ok  ",
                        CheckStatus::Warn => "warn",
                        CheckStatus::Fail => "FAIL",
                    };
                    format!("[{tag}] {:<12} {}", c.name, c.message)
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        // The report itself succeeded; exit 1 tells scripts that a check failed.
        if failed { ExitCode::from(1) } else { code }
    })
}
