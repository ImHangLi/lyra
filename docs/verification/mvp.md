# MVP verification index (LYR-18)

The final candidate is the tip of the PR stack. The stack runs #20 → #36, with this PR on top. Every row below points to the PR whose description records the live evidence. Results use four labels: **pass**, **fail**, **blocked**, and **not-run**. A "pass" with a note means the main case passed and the note names a part that was not run.

**Final regression** on candidate `74e99d0`, macOS 27.2 arm64:
- `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -D warnings`, `cargo build --locked`, and `scripts/check-contract.sh` all pass.
- One end-to-end CLI pass on the playground succeeded. It covered catalog, run, a failing run, up, start, logs, exec, a structured scan, view, publish, schedule, PTY input, apply, gc plan, storage status, doctor, the TUI first frame, and down.
- Nothing was left running, and there were no uncommitted files.

Performance numbers are in [performance.md](performance.md). Real-project records are in [onboarding.md](onboarding.md), and install checks are in [release.md](release.md).

| ID | Result | Evidence | Notes |
|---|---|---|---|
| TYPE-01 | pass | #20 | Misused ID types fail to compile; invalid strings give field errors |
| TYPE-02 | pass | #20 | A missing timeout gets the mode default; `null` is rejected; `after` range checked |
| TYPE-03 | pass | #25 | Invalid result combinations rejected; a process `result` is a protocol error |
| TYPE-04 | pass | #30 | The CLI and TUI apply top-level defaults the same way |
| PROTO-01 | pass | #25 | Byte and random chunk splits; UTF-8 intact |
| PROTO-02 | pass | #25 | Duplicate key, oversized frame, and truncated frame affect only that run |
| PROTO-03 | pass | #20 | Unknown fields, unknown tags, and external `$ref` are rejected with no network use |
| IPC-01 | pass | #21, #23, #28 | Parallel CLIs and two TUIs share one host and one run |
| IPC-02 | pass | #21 | Worktrees, CJK and space paths, and symlinks get the correct identity |
| IPC-03 | pass | #21 | Handshake mismatch and stale socket; the running host is never killed |
| IPC-04 | pass | #31 | 40 subscriptions during heavy churn; state was continuous or reset explicitly |
| IPC-05 | pass | #31 | A slow subscriber is reset; `stop` and `status` stay fast; the session survives |
| LIFE-01 | pass | #23, #28 | The last controller closing stops work; an extra controller keeps it running |
| LIFE-02 | pass | #27 | TTL expiry and non-overlapping ticks work. **not-run:** real OS sleep and wake |
| LIFE-03 | pass | #23 | Stopping one run leaves the others running |
| LIFE-04 | pass | #23, #29 | TERM, then KILL after grace, with reaping; a failed cleanup keeps the main result |
| LIFE-05 | pass | #34 | Compose worktrees and a port conflict: only its own resources are touched, and volumes are kept |
| IDEM-01 | pass | #23 | The same request key returns the same run and has no second side effect |
| IDEM-02 | pass | #32 | A crash after reservation gives `interrupted` with no rerun; history GC keeps dedupe |
| VIEW-01 | pass | #25 | An invalid table is rejected whole; the previous data stays and is marked stale |
| VIEW-02 | pass | #25, #30 | A stale row action gives VIEW_CHANGED with no side effect |
| VIEW-03 | pass | #31 | Large items and paging make progress; cleaned data gives PAYLOAD_GONE |
| VIEW-04 | pass | #25 | Publish works with no session; it is visible later as historical |
| VIEW-05 | pass | #32 | No revision reuse after SIGKILL; the old CAS is rejected |
| UI-01 | pass | #28 | Keys match the footer; key auto-repeat never triggers actions |
| UI-02 | pass | #28 | A pinned log stays put during a flood; copy is exact |
| UI-03 | pass | #30 | CJK, emoji, long lines, and resizes render without broken characters |
| UI-04 | pass | #28, #30 | 60×18, 80×24, and 120×40 are usable; smaller windows recover |
| UI-05 | pass | #30 | Mouse on and off match the help. **not-run:** native selection in Terminal.app or iTerm2 by hand |
| PTY-01 | pass | #29 | Esc and Ctrl-C reach the child; Ctrl-] detaches; paste arrives once |
| PTY-02 | pass | #29 | Input lock, INPUT_BUSY, release on disconnect, stale screen revision rejected |
| PTY-03 | pass | #29 | Only the lock holder resizes; alternate screen handled |
| PTY-04 | pass | #29 | OSC 52 and window sequences dropped; the host clipboard is unchanged |
| PTY-05 | pass | #29 | The terminal is restored after a UI error or panic; SIGKILL limits are documented |
| DATA-01 | pass | #32 | Per-run and workspace log quotas hold; the active run rotates |
| DATA-02 | pass | #22 | WAL is left to SQLite; clean close and reopen; a damaged DB is never replaced |
| DATA-03 | pass | #22, #32 | `stop` works with storage failing (4 MiB disk image). New actions are refused when a reservation cannot commit; this was verified at the storage layer |
| DATA-04 | pass | #23 | Secrets are never in history or errors; temporary inputs are deleted |
| DATA-05 | pass | #31 | Provenance flags a success from an older definition as `stale` |
| CONFIG-01 | pass | #26 | Of two concurrent applies, one succeeds and one gets REVISION_CONFLICT |
| CONFIG-02 | pass | #26 | Invalid or partial disk config blocks new invokes; `reload` recovers |
| CONFIG-03 | pass | #26 | A changed or disabled definition never restarts a run; it can still be stopped |
| AGENT-01 | pass | #33, #34 | Real agent setup on FastAPI and Outline |
| AGENT-02 | pass | #33, #34 | A new agent session reuses the existing tool |
| AGENT-03 | pass | #33 | The agent added a structured plugin with no Core change |
| AGENT-04 | pass | #31 | Reads are bounded by default and scoped to the run |
| VALUE-01 | pass | #34 | First setup gives runnable tools; what was not verified is listed |
| VALUE-02 | pass | #34 | The human stops the agent's backend run from the TUI |
| VALUE-03 | pass | #34 | A saved command is found by a new session without the old chat |
| VALUE-04 | pass | #33, #34 | A new plugin needed no rebuild; the TUI and CLI see the same data |
| VALUE-05 | pass | #27 | A deterministic schedule calls no model and stops with the session |
| VALUE-06 | pass | #31, #34 | Historical, stale, and cleaned data never read as current |

## Remaining limits

- **Blocked:** installing from a real GitHub release. No release was published without the owner's approval.
- **Not run:**
  - a real OS sleep and wake;
  - Terminal.app and iTerm2 by hand;
  - launching the x86_64 build (there was no Intel Mac and no Rosetta);
  - measurements on M2 hardware;
  - Outline startup (its dependencies were not installed);
  - Codex or generic agents.
- **No new tests:** no permanent automated tests were added, because the owner did not authorize them. The evidence is the recorded live runs and `scripts/check-contract.sh`.
