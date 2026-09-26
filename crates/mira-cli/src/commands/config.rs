//! `apply` and `reload`: accept a validated definition set without restarting running work.

use std::path::Path;
use std::process::ExitCode;

use mira_client::{ConnectOptions, MAINTENANCE_TIMEOUT};
use mira_protocol::ids::{AbsolutePath, CatalogRevision, RequestKey};
use mira_protocol::ipc::{ConfigApplied, ConfigApplyParams, Empty, Method};
use mira_protocol::reply::ReplyContext;

use super::ctx::{Ctx, block_on};
use crate::output::invalid_argument;

fn text(a: &ConfigApplied) -> String {
    format!(
        "catalog revision {}{}; plugins: {}",
        a.catalog_revision,
        if a.changed {
            " (changed)"
        } else {
            " (no change)"
        },
        a.plugins
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub fn apply(ctx: &Ctx, draft: &Path, expected: u64, request_key: Option<String>) -> ExitCode {
    block_on(async {
        let parsed = (|| {
            let real = std::fs::canonicalize(draft)
                .map_err(|e| invalid_argument(format!("{}: {e}", draft.display())))?;
            let draft_dir =
                AbsolutePath::from_path(&real).map_err(|e| invalid_argument(e.to_string()))?;
            let expected =
                CatalogRevision::new(expected).map_err(|e| invalid_argument(e.to_string()))?;
            let request_key = request_key
                .map(RequestKey::parse)
                .transpose()
                .map_err(|e| invalid_argument(e.to_string()))?;
            Ok::<_, mira_protocol::ErrorInfo>(ConfigApplyParams {
                draft_dir,
                expected_catalog_revision: expected,
                request_key,
            })
        })();
        let p = match parsed {
            Ok(p) => p,
            Err(e) => return ctx.fail(ReplyContext::default(), e),
        };
        let mut client = match ctx.client(&ConnectOptions::cli()).await {
            Ok(c) => c,
            Err((c, e)) => return ctx.fail(c, e),
        };
        match client
            .call_with_timeout::<_, ConfigApplied>(Method::ConfigApply, &p, MAINTENANCE_TIMEOUT)
            .await
        {
            Ok(r) => ctx.emit(&r, text),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}

pub fn reload(ctx: &Ctx) -> ExitCode {
    block_on(async {
        let mut client = match ctx.client(&ConnectOptions::cli()).await {
            Ok(c) => c,
            Err((c, e)) => return ctx.fail(c, e),
        };
        match client
            .call_with_timeout::<_, ConfigApplied>(
                Method::ConfigReload,
                &Empty {},
                MAINTENANCE_TIMEOUT,
            )
            .await
        {
            Ok(r) => ctx.emit(&r, text),
            Err(e) => ctx.fail(client.context(), e.to_error_info()),
        }
    })
}
