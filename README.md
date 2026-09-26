# Lyra

**Lyra is an agent-native, fully customizable control plane for your development workflow.**

See it, keep it, reuse it. You and your coding agent run, watch, and extend the same project tools from one place: you use a terminal workbench, the agent uses a structured CLI, and both drive the same host, runs, logs, and views. When a one-off command finally works, the agent saves it as a plugin, and the next agent finds and reuses it instead of rediscovering it.

- **One host per workspace.** Start the dev server from the TUI; your agent reads the same run's logs and stops it. No duplicate instances, no hidden shells.
- **Plugins are just files.** A `plugin.json` with commands, or a small script that prints typed tables, logs, and trees. New tools never need a Lyra rebuild.
- **Bounded, honest history.** Results keep their source run, time, and definition, and say when they are historical or stale.
- **No AI inside.** The Core runs no models and sends no telemetry. Your existing agent does setup and extension through the bundled skills.

macOS 14+, arm64 and x86_64. Status: MVP. What was verified, and what was not, is in [docs/verification/mvp.md](docs/verification/mvp.md); the demo path is in [docs/demo.md](docs/demo.md).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/ImHangLi/lyra/main/scripts/install.sh | sh
```

The installer puts one native binary in `~/.lyra/bin` (no sudo, no Node), verifies its SHA-256 checksum, and never replaces a `lyra` it did not install. From source: `cargo build --release --locked` (Rust toolchain pinned in `rust-toolchain.toml`).

## For agents

Give your agent this instruction in the project you want to set up:

> Set up Lyra for this repository. Run `lyra skills install --agent claude` (or `codex`/`generic`), then follow the installed `lyra` skill: run `lyra setup --json`, read the project's docs and scripts, create a few plugins for the real dev commands, validate and apply them, verify one, and tell me what is ready and what is not.

If your agent cannot load skills dynamically, have it read `.agents/skills/lyra/SKILL.md` directly. To add a tool later, ask for it in plain words; the `lyra-extend` skill saves it as a plugin and checks that the TUI and CLI both see it.

## For humans

Open `lyra` in the project. Move with `j`/`k`, run or start with `Enter`/`s`, read logs with `Tab`, and press `?` for every key. Closing the last window stops the work it owns; press `b` first to keep it running in the background (default 2 hours, or until `lyra down`).

## Try it

```sh
cd examples/playground
lyra                      # TUI: start dev.web, run dev.check, open demo views
lyra run demo.scan        # structured plugin: table, tree, and summary views
lyra view demo.table      # the same table the TUI shows
```

## Layout

| Path | Contents |
|---|---|
| `crates/lyra-protocol` | IDs, wire DTOs, validated domain types, errors, JSON Schemas |
| `crates/lyra-host` | Workspace host: actor, runners, SQLite ledger, socket server |
| `crates/lyra-client` | Typed client shared by CLI and TUI |
| `crates/lyra-tui` | Terminal workbench |
| `crates/lyra-discovery` | Static project facts for setup (never executes the project) |
| `crates/lyra-cli` | The `lyra` binary |
| `skills/` | Bundled agent skills (`lyra`, `lyra-extend`) |
| `examples/playground` | A small workspace with command, PTY, and structured plugins |
| `schemas/` | Generated JSON Schemas; `scripts/check-contract.sh` checks drift |

Contributor and agent rules: [AGENTS.md](AGENTS.md).
