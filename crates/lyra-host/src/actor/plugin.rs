//! LPP/1 run facts in the actor (§7.5, §7.6): results, reported health, progress, views, and
//! artifacts from validated frames, plus the task result precedence rules.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::hash::canonical_digest;
use lyra_protocol::ids::{PluginId, RunId, ViewId, ViewRef};
use lyra_protocol::lpp::{PluginEvent, PluginResult};
use lyra_protocol::manifest::ActionMode;
use lyra_protocol::run::{ExitInfo, LogStream, Outcome, ReportedHealth, RunResult, StopReason};
use lyra_protocol::schema_profile::SchemaDoc;
use lyra_protocol::time::Timestamp;
use lyra_protocol::view::{LogLevel, SourceKind};
use serde_json::{Value, json};

use super::Actor;

/// Result data larger than this is not kept inline in the run record (payload refs: LYR-07).
const MAX_INLINE_RESULT_BYTES: usize = 256 * 1024;

/// Per-run LPP/1 state. Present only for `run.kind = plugin` actions.
pub struct LppRun {
    pub mode: ActionMode,
    pub plugin: PluginId,
    pub output_schema: Option<SchemaDoc>,
    /// The resolved `context.cwd`; relative artifact paths resolve against it.
    pub cwd: PathBuf,
    pub artifact_dir: PathBuf,
    pub protocol_error: Option<ErrorInfo>,
    /// Views this run updated; they turn stale when the run fails.
    pub touched_views: BTreeSet<ViewRef>,
    /// Latest unsent progress (coalesced, sent on the next tick).
    pub progress: Option<(String, Option<(f64, f64)>)>,
    pub artifacts: u32,
}

impl LppRun {
    pub fn new(mode: ActionMode, plugin: PluginId, output_schema: Option<SchemaDoc>) -> Self {
        Self {
            mode,
            plugin,
            output_schema,
            cwd: PathBuf::new(),
            artifact_dir: PathBuf::new(),
            protocol_error: None,
            touched_views: BTreeSet::new(),
            progress: None,
            artifacts: 0,
        }
    }
}

/// The final outcome of a plugin run when no stop request or spawn failure decides it.
pub struct Verdict {
    pub outcome: Outcome,
    pub stop_reason: Option<StopReason>,
    pub note: Option<String>,
}

/// §7.6: a task succeeds only with one valid success result as its last frame and exit 0.
pub fn verdict(
    lpp: &LppRun,
    result: Option<&RunResult>,
    exit: &Option<ExitInfo>,
) -> Option<Verdict> {
    if lpp.mode == ActionMode::Process {
        return None;
    }
    let exit0 = matches!(exit, Some(ExitInfo { code: Some(0), .. }));
    Some(match result {
        None => Verdict {
            outcome: Outcome::Failed,
            stop_reason: Some(StopReason::ProtocolError),
            note: Some(
                "protocol error [INVALID_FRAME]: the task ended without a result frame".into(),
            ),
        },
        Some(r) if !r.ok => Verdict {
            outcome: Outcome::Failed,
            stop_reason: None,
            note: None,
        },
        Some(_) if exit0 => Verdict {
            outcome: Outcome::Succeeded,
            stop_reason: None,
            note: None,
        },
        Some(_) => Verdict {
            outcome: Outcome::Failed,
            stop_reason: None,
            note: Some(match exit {
                Some(ExitInfo { code: Some(c), .. }) => {
                    format!(
                        "the plugin reported success, then exited with status {c}; the run failed"
                    )
                }
                Some(ExitInfo {
                    signal: Some(s), ..
                }) => {
                    format!("the plugin reported success, then was ended by {s}; the run failed")
                }
                _ => "the plugin reported success, but its exit status is unknown; the run failed"
                    .into(),
            }),
        },
    })
}

pub fn append_note(note: &mut Option<String>, text: &str) {
    *note = Some(match note.take() {
        Some(n) => format!("{n}; {text}"),
        None => text.to_owned(),
    });
}

impl Actor {
    /// Writes one host line into a run's log and streams it.
    pub(crate) fn host_note(&mut self, run_id: &RunId, level: LogLevel, text: &str) {
        let records = match self.runs.get(run_id).map(|r| r.log.clone()) {
            Some(log) => match log.lock() {
                Ok(mut l) => l.push_line(LogStream::Host, level, text, false),
                Err(_) => return,
            },
            None => return,
        };
        self.broadcast_logs(run_id, records);
    }

    pub(super) fn plugin_event(&mut self, run_id: RunId, ev: PluginEvent) {
        let Some(run) = self.runs.get_mut(&run_id) else {
            return;
        };
        let Some(lpp) = run.lpp.as_mut() else {
            return;
        };
        if lpp.protocol_error.is_some() {
            return;
        }
        match ev {
            // The runner writes log frames to the run log directly.
            PluginEvent::Log { .. } => {}
            PluginEvent::Progress { message, current } => lpp.progress = Some((message, current)),
            PluginEvent::Status { state, message } => {
                run.record.reported_health = ReportedHealth {
                    state,
                    message: Some(message),
                    updated_at: Some(Timestamp::now()),
                };
                self.state_changed();
            }
            PluginEvent::View { view_id, op, data } => {
                let view_ref = ViewRef::new(lpp.plugin.clone(), view_id);
                self.enqueue_view_update(view_ref, op, data, Some(run_id), SourceKind::Plugin);
            }
            PluginEvent::Artifact {
                path,
                mime,
                label,
                ownership,
            } => {
                lpp.artifacts += 1;
                let (n, cwd, dir) = (lpp.artifacts, lpp.cwd.clone(), lpp.artifact_dir.clone());
                let registered = self
                    .artifacts
                    .register(&run_id, n, &cwd, &dir, &path, mime, label, ownership);
                if let Err(e) = registered {
                    self.protocol_error(&run_id, e, None);
                }
            }
            PluginEvent::Result(res) => self.plugin_result(&run_id, res),
        }
    }

    fn plugin_result(&mut self, run_id: &RunId, res: PluginResult) {
        let (ok, summary, data, error) = match res {
            PluginResult::Success { summary, data } => (true, summary, data, None),
            PluginResult::Failure {
                summary,
                data,
                error,
            } => (false, summary, data, Some(error)),
        };
        let schema = self
            .runs
            .get(run_id)
            .and_then(|r| r.lpp.as_ref())
            .and_then(|l| l.output_schema.clone());
        let mut schema_error = None;
        if ok && let Some(schema) = schema {
            match self.output_validator(&schema) {
                Ok(v) => {
                    let issues = schema.validate(&v, &data, "/data");
                    if !issues.is_empty() {
                        let info = issues.to_error_info();
                        schema_error = Some(ErrorInfo {
                            message: format!(
                                "the success result does not match output_schema: {}",
                                info.message
                            ),
                            ..info
                        });
                    }
                }
                Err(e) => schema_error = Some(e),
            }
        }
        let size = serde_json::to_vec(&data).map_or(0, |v| v.len());
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        let data = if size > MAX_INLINE_RESULT_BYTES {
            append_note(
                &mut run.record.note,
                &format!(
                    "result data ({size} bytes) exceeds the inline bound of {} KiB and was not kept",
                    MAX_INLINE_RESULT_BYTES / 1024
                ),
            );
            Value::Null
        } else {
            data
        };
        run.record.result = Some(RunResult {
            ok,
            summary,
            data,
            payload: None,
            error,
        });
        if let Some(e) = schema_error {
            self.protocol_error(run_id, e, None);
        }
        self.state_changed();
    }

    fn output_validator(
        &mut self,
        schema: &SchemaDoc,
    ) -> Result<std::sync::Arc<jsonschema::Validator>, ErrorInfo> {
        let key = canonical_digest(&json!({"output_schema": schema.as_map()}))
            .map_err(|e| ErrorInfo::new(ErrorCode::INTERNAL, e))?;
        if let Some(v) = self.validators.get(&key) {
            return Ok(v.clone());
        }
        let v = std::sync::Arc::new(
            schema
                .compile()
                .map_err(|e| ErrorInfo::new(ErrorCode::SCHEMA_INVALID, e))?,
        );
        self.validators.insert(key, v.clone());
        Ok(v)
    }

    /// Stops only this run; its earlier valid views stay readable and turn stale (§7.6).
    pub(crate) fn protocol_error(
        &mut self,
        run_id: &RunId,
        error: ErrorInfo,
        view_hint: Option<ViewId>,
    ) {
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        let Some(lpp) = run.lpp.as_mut() else {
            return;
        };
        if lpp.protocol_error.is_some() {
            return;
        }
        let text = match &view_hint {
            Some(v) => format!(
                "protocol error [{}] in view `{v}`: {}",
                error.code, error.message
            ),
            None => format!("protocol error [{}]: {}", error.code, error.message),
        };
        lpp.protocol_error = Some(error);
        lpp.progress = None;
        let plugin = lpp.plugin.clone();
        append_note(&mut run.record.note, &text);
        // The failed output cannot be trusted to name its target: every view of the plugin
        // keeps its last valid content and turns stale.
        let reason = format!("run {run_id} of this plugin stopped on a protocol error");
        self.mark_plugin_views_stale(&plugin, &reason);
        self.stop_run(run_id, StopReason::ProtocolError);
        self.state_changed();
    }

    /// Sends the latest coalesced progress of each plugin run.
    pub(super) fn flush_progress(&mut self) {
        let pending: Vec<_> = self
            .runs
            .iter_mut()
            .filter_map(|(id, r)| {
                let p = r.lpp.as_mut()?.progress.take()?;
                Some((id.clone(), p))
            })
            .collect();
        for (run_id, (message, current)) in pending {
            self.broadcast_progress(&run_id, message, current);
        }
    }
}
