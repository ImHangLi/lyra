//! Stream subscriptions (§9.3, §11.3, §11.6). Each subscriber has a bounded queue; a slow
//! subscriber is reset (RESET_REQUIRED) without affecting control connections or the session.

use std::collections::BTreeSet;

use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::ids::{ClientId, RunId, SubscriptionId};
use lyra_protocol::ipc::*;
use lyra_protocol::reply::ReplyMeta;
use lyra_protocol::run::LogRecord;
use lyra_protocol::time::Timestamp;

use super::{Actor, Outbound, Responder};

/// Log events carry at most this many serialized bytes of records.
const LOG_BATCH_BYTES: usize = 32 * 1024;

pub struct Sub {
    pub client: ClientId,
    pub kinds: BTreeSet<StreamKind>,
    pub refs: Vec<String>,
    pub out: Outbound,
}

impl Actor {
    fn frame(
        &mut self,
        run_id: Option<RunId>,
        item_ref: Option<lyra_protocol::ids::ItemRef>,
        event: StreamEvent,
    ) -> Vec<u8> {
        if let Some(n) = self.event_seq.next() {
            self.event_seq = n;
        }
        let frame = StreamFrame {
            api: lyra_protocol::ids::Api1,
            host_epoch: self.epoch.clone(),
            event_seq: self.event_seq,
            recorded_at: Timestamp::now(),
            cursor: format!("{}.{}", self.epoch, self.event_seq),
            source: StreamSource {
                workspace_id: self.paths.id.clone(),
                run_id,
                item_ref,
            },
            event,
        };
        let note = RpcNotification {
            jsonrpc: JsonRpc2,
            method: STREAM_EVENT.to_owned(),
            params: frame,
        };
        let mut line = serde_json::to_vec(&note).unwrap_or_default();
        line.push(b'\n');
        line
    }

    pub(super) fn subscribe(&mut self, client: &ClientId, p: StreamSubscribeParams, r: Responder) {
        if p.kinds.is_empty() {
            return r.send(self.fail(ErrorInfo::new(
                ErrorCode::INVALID_ARGUMENT,
                "subscribe needs at least one kind",
            )));
        }
        if p.cursor.is_some() {
            // Live cursors do not survive a new subscription; the client re-reads a snapshot.
            return r.send(self.fail(ErrorInfo::new(
                ErrorCode::RESET_REQUIRED,
                "resume by cursor is not supported; take a new snapshot",
            )));
        }
        let Some(out) = self.clients.get(client).map(|c| c.out.clone()) else {
            return;
        };
        let id = SubscriptionId::random();
        let ready = StreamEvent::Ready {
            subscription_id: id.clone(),
            catalog_revision: self.catalog_revision,
            state_revision: self.state_revision,
        };
        let snapshot = StreamEvent::Snapshot(self.status_data());
        // Reply, ready, and snapshot are produced in one actor step: no event can slip between.
        r.send(self.ok(
            Subscribed {
                subscription_id: id.clone(),
            },
            ReplyMeta::default(),
        ));
        let ready = self.frame(None, None, ready);
        let snapshot = self.frame(None, None, snapshot);
        let ok = out.event(ready) && out.event(snapshot);
        let sub = Sub {
            client: client.clone(),
            kinds: Actor::kinds_of(&p.kinds),
            refs: p.refs,
            out,
        };
        if ok {
            self.subs.insert(id, sub);
        }
    }

    pub(super) fn unsubscribe(
        &mut self,
        client: &ClientId,
        p: StreamUnsubscribeParams,
        r: Responder,
    ) {
        let known = self
            .subs
            .get(&p.subscription_id)
            .is_some_and(|s| &s.client == client);
        if known && let Some(sub) = self.subs.remove(&p.subscription_id) {
            let end = self.frame(
                None,
                None,
                StreamEvent::End {
                    reason: EndReason::Unsubscribed,
                },
            );
            sub.out.respond(end);
        }
        r.send(self.ok(Ack { ok: known }, ReplyMeta::default()));
    }

    /// Sends to matching subscribers; resets any whose queue is full.
    fn deliver(&mut self, kind: StreamKind, matches: impl Fn(&Sub) -> bool, line: &[u8]) {
        let mut reset = Vec::new();
        for (id, sub) in &self.subs {
            if sub.kinds.contains(&kind) && matches(sub) && !sub.out.event(line.to_vec()) {
                reset.push(id.clone());
            }
        }
        for id in reset {
            if let Some(sub) = self.subs.remove(&id) {
                let end = self.frame(
                    None,
                    None,
                    StreamEvent::End {
                        reason: EndReason::ResetRequired,
                    },
                );
                // The control-priority channel is unbounded, so the reset notice is not lost.
                sub.out.respond(end);
            }
        }
    }

    pub(crate) fn broadcast_state(&mut self) {
        if !self
            .subs
            .values()
            .any(|s| s.kinds.contains(&StreamKind::State))
        {
            return;
        }
        let status = self.status_data();
        let ev = StreamEvent::State {
            state_revision: self.state_revision,
            session: status.session,
            runs: status.runs,
            storage_warnings: status.storage_warnings,
        };
        let line = self.frame(None, None, ev);
        self.deliver(StreamKind::State, |_| true, &line);
    }

    /// A view got a new revision; subscribers read the content with `view.read`.
    pub(crate) fn broadcast_view(
        &mut self,
        view_ref: &lyra_protocol::ids::ViewRef,
        view_revision: lyra_protocol::ids::ViewRevision,
    ) {
        if !self
            .subs
            .values()
            .any(|s| s.kinds.contains(&StreamKind::View))
        {
            return;
        }
        let key = view_ref.to_string();
        let line = self.frame(
            None,
            Some(view_ref.to_item_ref()),
            StreamEvent::View {
                view_ref: view_ref.clone(),
                view_revision,
            },
        );
        self.deliver(
            StreamKind::View,
            |s| s.refs.is_empty() || s.refs.iter().any(|r| r == &key),
            &line,
        );
    }

    /// Coalesced plugin progress (the latest value per tick).
    pub(crate) fn broadcast_progress(
        &mut self,
        run_id: &RunId,
        message: String,
        current: Option<(f64, f64)>,
    ) {
        if !self
            .subs
            .values()
            .any(|s| s.kinds.contains(&StreamKind::Progress))
        {
            return;
        }
        let action = self
            .runs
            .get(run_id)
            .and_then(|r| r.record.action_ref.clone());
        let keys = [
            Some(run_id.to_string()),
            action.as_ref().map(ToString::to_string),
        ];
        let line = self.frame(
            Some(run_id.clone()),
            action.as_ref().map(|a| a.to_item_ref()),
            StreamEvent::Progress {
                run_id: run_id.clone(),
                message,
                current: current.map(|c| c.0),
                total: current.map(|c| c.1),
            },
        );
        self.deliver(
            StreamKind::Progress,
            |s| s.refs.is_empty() || s.refs.iter().any(|r| keys.iter().flatten().any(|k| k == r)),
            &line,
        );
    }

    pub(crate) fn broadcast_logs(&mut self, run_id: &RunId, records: Vec<LogRecord>) {
        if records.is_empty()
            || !self
                .subs
                .values()
                .any(|s| s.kinds.contains(&StreamKind::Log))
        {
            return;
        }
        let action = self
            .runs
            .get(run_id)
            .and_then(|r| r.record.action_ref.clone());
        let keys = [
            Some(run_id.to_string()),
            action.as_ref().map(ToString::to_string),
        ];
        let mut batch = Vec::new();
        let mut size = 0usize;
        let mut batches = Vec::new();
        for r in records {
            let n = serde_json::to_vec(&r).map(|v| v.len()).unwrap_or(0);
            if size + n > LOG_BATCH_BYTES && !batch.is_empty() {
                batches.push(std::mem::take(&mut batch));
                size = 0;
            }
            size += n;
            batch.push(r);
        }
        batches.push(batch);
        for records in batches {
            let item = action.as_ref().map(|a| a.to_item_ref());
            let line = self.frame(
                Some(run_id.clone()),
                item,
                StreamEvent::Log {
                    run_id: run_id.clone(),
                    records,
                },
            );
            self.deliver(
                StreamKind::Log,
                |s| {
                    s.refs.is_empty()
                        || s.refs.iter().any(|r| keys.iter().flatten().any(|k| k == r))
                },
                &line,
            );
        }
    }
}
