# AGENTS.md

Lyra is an agent-native, customizable control plane for a development workflow. One native `lyra` binary gives humans a TUI and agents a structured CLI. Both talk to one host per workspace. The Core runs no models and has no client business logic.

## Sources of truth

- `docs/LYRA_IMPLEMENTATION_SPEC.md` is the product contract. `docs/LYRA_DELIVERY_PLAN.md` holds tickets LYR-01..18. Both are local-only (gitignored) and written in Chinese. Never commit, quote at length, or publish them.
- GitHub issues carry the English ticket text. Read the ticket, then only the spec sections it lists (`grep -n '^#' docs/LYRA_IMPLEMENTATION_SPEC.md` for offsets).
- MUST items in the spec are fixed: field tables, defaults, error codes, CLI semantics, state machines, limits. Change a MUST only with an explicit spec erratum in the PR. SHOULD items may improve with a recorded reason.

## Layout

Cargo workspace, Rust stable 2024 edition, pinned in `rust-toolchain.toml`.

| Crate | Owns | Must not depend on |
|---|---|---|
| `lyra-protocol` | IDs, wire DTOs, validated domain types, errors, strict JSON, schema export | Tokio, TUI, SQLite |
| `lyra-discovery` | Static project facts; never executes project code | runners, host |
| `lyra-host` | Workspace actor, storage thread, runners, scheduler, socket server | TUI |
| `lyra-client` | Typed LIPC/1 client used by both CLI and TUI | host internals |
| `lyra-tui` | Ratatui presentation state only | SQLite, spawning |
| `lyra-cli` | `lyra` binary: clap parsing, output rendering, wiring | — |

The workspace actor is the only writer of run state. TUI and CLI never write SQLite or spawn project commands. No plugin, framework, or client names in Core `if/else`.

## Code rules

- Raw bytes → wire DTO → validated domain → effect. `serde_json::Value` only in open slots (plugin `config`/`meta`/`input`/`result.data`, json views, schemas, error details).
- Distinct newtypes for every ID and revision. Enums, not boolean combinations. Validated types have no bypassing `Deserialize`.
- No `unwrap`/`expect`/`panic!` on reachable input paths. `#![forbid(unsafe_code)]` outside one small platform module.
- Never log or `Debug`-print env values, stdin, plugin config, or PTY input.
- No generic repository/service/factory layers.

## Checks

- `cargo fmt --all` and `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- Run builds and full checks through the shared lock, one at a time on this machine:
  `python3 ~/.codex/scripts/with-heavy-check-lock.py -- cargo build --workspace --locked`
  Exit code 75 means the slot is busy: do other work, then retry.
- Verify with live smoke checks on the real binary in a scratch workspace under `/tmp`. Clean up processes, sockets, and files you created.
- Do not add permanent tests unless the owner authorizes it. Fixture data for `validate`/examples is allowed.

## Git

- Branch `leon/lyr-XX-<slug>` from `main`. Conventional Commit PR titles, e.g. `feat(runtime): run and stop managed project commands`.
- PR body follows `.github/pull_request_template.md` with `Closes #N` and real evidence. Squash merge.
- English only in code, docs, commits, issues, and PRs. No credentials or private data. No agent author or co-author attribution.
