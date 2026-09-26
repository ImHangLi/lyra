//! Typed views, publishes, row actions, and artifacts (§8, §10.2). The CLI renders the same
//! ViewData the host stores; it never re-runs a plugin to read a view.

use std::process::ExitCode;

use mira_protocol::error::ErrorInfo;
use mira_protocol::ids::{ActionId, RequestKey, RunId, ViewRef, ViewRevision};
use mira_protocol::ipc::*;
use mira_protocol::reply::ReplyContext;
use mira_protocol::view::{Freshness, SourceKind, TreeNode, ViewBody, ViewData, ViewSnapshot};
use serde_json::Value;

use super::ctx::{Ctx, block_on};
use super::runtime::{connect, env, read_input, run_and_wait};
use crate::output::invalid_argument;

fn parse_view(s: &str) -> Result<ViewRef, ErrorInfo> {
    ViewRef::parse(s.to_owned()).map_err(|e| invalid_argument(format!("{e}: `{s}`")))
}

fn parse_revision(n: u64) -> Result<ViewRevision, ErrorInfo> {
    ViewRevision::new(n).map_err(|e| invalid_argument(e.to_string()))
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn tree_lines(nodes: &[TreeNode], depth: usize, out: &mut Vec<String>) {
    for n in nodes {
        out.push(format!("{}{}", "  ".repeat(depth), n.label));
        tree_lines(&n.children, depth + 1, out);
    }
}

fn data_text(d: &ViewData) -> String {
    match d {
        ViewData::Text { text, .. } => text.clone(),
        ViewData::Table { columns, rows } => {
            let mut lines = vec![format!(
                "id\t{}",
                columns
                    .iter()
                    .map(|c| c.label.as_str())
                    .collect::<Vec<_>>()
                    .join("\t")
            )];
            for r in rows {
                let cells: Vec<String> = columns
                    .iter()
                    .map(|c| r.values.get(&c.id).map(cell).unwrap_or_default())
                    .collect();
                lines.push(format!("{}\t{}", r.id, cells.join("\t")));
            }
            lines.join("\n")
        }
        ViewData::Log { items } => items
            .iter()
            .map(|i| {
                format!(
                    "{} {:<5} {}",
                    i.recorded_at.map(|t| t.to_string()).unwrap_or_default(),
                    format!("{:?}", i.level).to_lowercase(),
                    i.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        ViewData::Tree { nodes } => {
            let mut out = Vec::new();
            tree_lines(nodes, 0, &mut out);
            out.join("\n")
        }
        ViewData::Json { value } => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
    }
}

fn freshness_text(f: Freshness) -> &'static str {
    match f {
        Freshness::Current => "current",
        Freshness::Historical => "historical",
        Freshness::Stale => "stale",
    }
}

fn snapshot_text(s: &ViewSnapshot) -> String {
    let mut head = format!(
        "{}  {}",
        s.view_ref,
        s.view_revision
            .map_or_else(|| "no data".to_owned(), |r| format!("revision {r}"))
    );
    head.push_str(&format!("  {}", freshness_text(s.freshness)));
    if let Some(reason) = &s.freshness_reason {
        head.push_str(&format!(" ({reason})"));
    }
    if let Some(at) = s.recorded_at {
        head.push_str(&format!("\n  recorded {at}"));
    }
    let body = match &s.data {
        Some(ViewBody::Inline(d)) => data_text(d),
        Some(ViewBody::Reference(r)) => r.summary.clone(),
        None => String::new(),
    };
    if body.is_empty() {
        head
    } else {
        format!("{head}\n\n{body}")
    }
}

pub fn view(
    ctx: &Ctx,
    view: &str,
    after: Option<String>,
    limit: Option<u32>,
    max_bytes: Option<u32>,
) -> ExitCode {
    block_on(async {
        let view_ref = match parse_view(view) {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = ViewReadParams {
            view_ref,
            cursor: after,
            limit,
            max_bytes,
        };
        match client.call::<_, ViewSnapshot>(Method::ViewRead, &p).await {
            Ok(r) => ctx.emit(&r, snapshot_text),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn publish(
    ctx: &Ctx,
    view: &str,
    input: &str,
    expected: Option<u64>,
    request_key: Option<String>,
) -> ExitCode {
    block_on(async {
        let prepared = (|| {
            Ok::<_, ErrorInfo>((
                parse_view(view)?,
                read_input(Some(input))?,
                expected.map(parse_revision).transpose()?,
                request_key
                    .map(RequestKey::parse)
                    .transpose()
                    .map_err(|e| invalid_argument(e.to_string()))?,
            ))
        })();
        let (view_ref, frame, expected_view_revision, request_key) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = ViewPublishParams {
            view_ref,
            frame,
            source_kind: SourceKind::Cli,
            expected_view_revision,
            request_key,
        };
        match client
            .call::<_, PublishResult>(Method::ViewPublish, &p)
            .await
        {
            Ok(r) => ctx.emit(&r, |d| {
                format!(
                    "{}  revision {}  {}{}",
                    d.view_ref,
                    d.view_revision,
                    serde_json::to_value(d.durability)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    if d.reused { "  (reused)" } else { "" }
                )
            }),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn view_action(ctx: &Ctx, view: &str, action: &str, row: String, expected: u64) -> ExitCode {
    block_on(async {
        let prepared = (|| {
            Ok::<_, ErrorInfo>((
                parse_view(view)?,
                ActionId::parse(action.to_owned())
                    .map_err(|e| invalid_argument(format!("{e}: `{action}`")))?,
                parse_revision(expected)?,
                env()?,
            ))
        })();
        let (view_ref, action, expected_view_revision, client_env) = match prepared {
            Ok(v) => v,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = ViewActionParams {
            view_ref,
            action,
            row,
            expected_view_revision,
            client_env,
        };
        let params = serde_json::to_value(p).unwrap_or(Value::Null);
        run_and_wait(ctx, &mut client, Method::ViewAction, params, true).await
    })
}

fn artifact_text(a: &ArtifactInfo) -> String {
    format!(
        "{}  {}  {}  {}  {}{}",
        a.id,
        serde_json::to_value(a.ownership)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        serde_json::to_value(a.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        a.size_bytes
            .map_or_else(|| "-".to_owned(), |b| format!("{b} B")),
        a.path,
        if a.label.is_empty() {
            String::new()
        } else {
            format!("  ({})", a.label)
        }
    )
}

pub fn artifacts(ctx: &Ctx, run: Option<String>) -> ExitCode {
    block_on(async {
        let run_id = match run.map(RunId::parse).transpose() {
            Ok(r) => r,
            Err(e) => return ctx.fail(ReplyContext::default(), invalid_argument(e.to_string())),
        };
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        match client
            .call::<_, ArtifactListData>(Method::ArtifactList, &ArtifactListParams { run_id })
            .await
        {
            Ok(r) => ctx.emit(&r, |d| {
                if d.artifacts.is_empty() {
                    "no artifacts".into()
                } else {
                    d.artifacts
                        .iter()
                        .map(artifact_text)
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn artifact_read(ctx: &Ctx, id: String, offset: u64, max_bytes: Option<u32>) -> ExitCode {
    block_on(async {
        let mut client = match connect(ctx).await {
            Ok(c) => c,
            Err(code) => return code,
        };
        let p = ArtifactReadParams {
            artifact_id: id,
            offset,
            max_bytes,
        };
        match client.call::<_, ChunkData>(Method::ArtifactRead, &p).await {
            Ok(r) => ctx.emit(&r, |c| c.text.clone()),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}
