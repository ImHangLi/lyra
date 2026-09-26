# MVP verification index (LYR-18)

The stack (#20 → #39, then #41) was merged into `main` as GitHub stack #40 at `039c0c3`, and `v0.1.0` was tagged on that commit. Every row below points to the PR whose description records the live evidence. Results use five labels: **pass**, **partial**, **fail**, **blocked**, and **not-run**. **partial** means the main case passed, but part of the pass condition was not run. A note on a "pass" row gives the scope of the evidence.

**Final regression** on candidate `74e99d0`, macOS 27.2 arm64:
- `cargo fmt --check`, `cargo clippy --workspace --all-targets --locked -D warnings`, `cargo build --locked`, and `scripts/check-contract.sh` all pass.
- One end-to-end CLI pass on the test workspace succeeded. It covered catalog, run, a failing run, up, start, logs, exec, a structured scan, view, publish, schedule, PTY input, apply, gc plan, storage status, doctor, the TUI first frame, and down.
- Nothing was left running, and there were no uncommitted files.

**After the merge**, on `main` at `039c0c3`: CI passed (format, clippy, build, contract, and a live smoke). The same checks and a CLI pass on the test workspace passed locally, and the released binary passed the install and first-use checks in [release.md](release.md).

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
| LIFE-02 | partial | #27 | TTL expiry and non-overlapping ticks work with shortened TTLs. **not-run:** real OS sleep and wake, which is part of the pass condition |
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
| UI-02 | partial | #28 | A pinned log stays put during a flood, and copy is exact. Checked in a PTY with an xterm emulator (pyte). **not-run:** copy and scroll in a real macOS terminal |
| UI-03 | pass | #30 | CJK, emoji, long lines, and resizes render without broken characters |
| UI-04 | pass | #28, #30 | 60×18, 80×24, and 120×40 are usable; smaller windows recover |
| UI-05 | partial | #30 | Mouse on and off match the help in an emulated PTY. **not-run:** native selection in Terminal.app or iTerm2 by hand |
| PTY-01 | pass | #29 | Esc and Ctrl-C reach the child; Ctrl-] detaches; paste arrives once |
| PTY-02 | pass | #29 | Input lock, INPUT_BUSY, release on disconnect, stale screen revision rejected |
| PTY-03 | pass | #29 | Only the lock holder resizes; alternate screen handled |
| PTY-04 | pass | #29 | OSC 52 and window sequences dropped; the host clipboard is unchanged |
| PTY-05 | partial | #29 | The terminal is restored after a UI error or panic in an emulated PTY; SIGKILL limits are documented. **not-run:** window close in a real macOS terminal |
| DATA-01 | pass | #32 | Per-run and workspace log quotas hold; the active run rotates |
| DATA-02 | pass | #22 | WAL is left to SQLite; clean close and reopen; a damaged DB is never replaced |
| DATA-03 | partial | #22, #32 | `stop` works end to end with storage failing (4 MiB disk image). New actions are refused when a reservation cannot commit, but this was verified at the storage layer only, not end to end |
| DATA-04 | pass | #23 | Secrets are never in history or errors; temporary inputs are deleted |
| DATA-05 | pass | #31 | Provenance flags a success from an older definition as `stale` |
| CONFIG-01 | pass | #26 | Of two concurrent applies, one succeeds and one gets REVISION_CONFLICT |
| CONFIG-02 | pass | #26 | Invalid or partial disk config blocks new invokes; `reload` recovers |
| CONFIG-03 | pass | #26 | A changed or disabled definition never restarts a run; it can still be stopped |
| AGENT-01 | pass | #33, #34 | Real agent (Claude) setup on FastAPI, including a run. On Outline the setup was checked for structure only |
| AGENT-02 | pass | #33, #34 | A new agent session reuses the existing tool |
| AGENT-03 | pass | #33 | The agent added a structured plugin with no Core change |
| AGENT-04 | pass | #31 | Reads are bounded by default and scoped to the run |
| VALUE-01 | pass | #34 | First setup gives runnable tools on FastAPI; what was not verified is listed. Outline tools were not started |
| VALUE-02 | pass | #34 | The human stops the agent's backend run from the TUI |
| VALUE-03 | pass | #34 | A saved command is found by a new session without the old chat |
| VALUE-04 | pass | #33, #34 | A new plugin needed no rebuild; the TUI and CLI see the same data |
| VALUE-05 | pass | #27 | A deterministic schedule calls no model and stops with the session |
| VALUE-06 | pass | #31, #34 | Historical, stale, and cleaned data never read as current |

## Remaining limits

- **Not run:**
  - A real OS sleep and wake. It needs the machine to sleep and a physical wake. The TTL logic was checked with shortened TTLs (LIFE-02).
  - Terminal.app and iTerm2 by hand. The agent could not drive or capture a real window: Terminal automation waits for an Automation permission prompt, `screencapture` has no Screen Recording permission, and iTerm2 is not installed. The TUI was checked in a real PTY with an xterm emulator instead.
  - Outline startup. The Docker engine on the verification machine returned HTTP 500 for every API call, and a full Outline install needs several GB of dependencies.
  - Agent setup with Codex or a generic agent. `skills install --agent codex` and `--agent generic` write the skills to `.agents/skills`, but a headless agent run was not permitted in this environment. Setup and reuse were verified with Claude only.
- **Out of scope:** the x86_64 build and M2 hardware. Every target machine is Apple Silicon M3 or later.
- **Few automated tests:** the owner authorized a small set after the release review. They cover the fixes in #43 and the process-group ledger: bounded log tails, env-file errors that hide values, `input --text` parsing, and never signalling a mismatched process. CI runs them. The other evidence is the recorded live runs, CI, and `scripts/check-contract.sh`.
