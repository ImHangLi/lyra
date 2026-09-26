//! Shared workspace selection and host connection for commands that need the host.

use std::path::PathBuf;
use std::process::ExitCode;

use lyra_client::{Client, ConnectOptions, connect};
use lyra_protocol::ErrorInfo;
use lyra_protocol::paths::WorkspacePaths;
use lyra_protocol::reply::{PublicReply, ReplyContext};
use lyra_protocol::workspace::{Selected, select};
use serde::Serialize;

use crate::output::{self, Mode};

pub struct Ctx {
    pub mode: Mode,
    pub project: Option<PathBuf>,
}

impl Ctx {
    pub fn select(&self) -> Result<Selected, ErrorInfo> {
        let cwd = std::env::current_dir()
            .map_err(|e| ErrorInfo::new(lyra_protocol::ErrorCode::NOT_FOUND, e.to_string()))?;
        select(self.project.as_deref(), &cwd).map_err(|e| e.to_error_info())
    }

    pub fn paths(&self) -> Result<WorkspacePaths, ErrorInfo> {
        self.select().map(|s| WorkspacePaths::new(s.root))
    }

    /// Selects the workspace and connects as an observer control client.
    pub async fn client(&self, opts: &ConnectOptions) -> Result<Client, (ReplyContext, ErrorInfo)> {
        let paths = self.paths().map_err(|e| (ReplyContext::default(), e))?;
        let ctx = ReplyContext {
            workspace: Some(lyra_protocol::reply::WorkspaceRef {
                id: paths.id.clone(),
                root: paths.root.clone(),
            }),
            ..ReplyContext::default()
        };
        connect(&paths, opts)
            .await
            .map_err(|e| (ctx, e.to_error_info()))
    }

    pub fn fail(&self, ctx: ReplyContext, e: ErrorInfo) -> ExitCode {
        output::fail(self.mode, ctx, e)
    }

    pub fn emit<T: Serialize>(
        &self,
        reply: &PublicReply<T>,
        text: impl FnOnce(&T) -> String,
    ) -> ExitCode {
        output::emit(self.mode, reply, text)
    }
}

/// Runs an async command body on a small current-thread runtime.
pub fn block_on<F: std::future::Future<Output = ExitCode>>(f: F) -> ExitCode {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt.block_on(f),
        Err(_) => ExitCode::from(7),
    }
}
