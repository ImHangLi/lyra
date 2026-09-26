<p align="center">
  <img src=".github/assets/lyra-hero.png" alt="Lyra" width="100%">
</p>

<h1 align="center">Lyra</h1>

<h3 align="center">One place for you and your agent.</h3>

<p align="center">
  <a href="https://github.com/ImHangLi/lyra/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/ImHangLi/lyra?style=flat-square&color=f26b3a"></a>
  <img alt="Platform" src="https://img.shields.io/badge/macOS-arm64-f26b3a?style=flat-square">
</p>

---

Your agents do the work. You still have to find it, check it, and explain it again tomorrow.

**Lyra gives you and your agents one shared place to run your work. You shape it to fit how you work, and both of you can see what is happening.**

- **See it for yourself.** Every run, log, port, and result is on one screen. Your agent sees exactly the same thing through the CLI.
- **Shaped around your work.** Your agent turns any workflow into a tool: dev servers and tests, Slack routines, video exports. No Lyra rebuild, no config language to learn.
- **Keep what worked.** A command that worked once becomes a saved tool. The next agent session, and the next engineer on the repo, finds it.

<p align="center"><img src=".github/assets/lyra-architecture.png" alt="How Lyra works: the human uses the TUI and agents use the CLI. Both talk to one Lyra host per workspace over LIPC/1 (JSON-RPC 2.0 on a Unix socket) and receive stream events. The host owns runs (pipe or PTY), plugins (LPP/1 JSONL), a SQLite ledger in WAL mode, and typed views (table, tree, log, text, JSON)." width="100%"></p>

## What it fixes

**For engineers**

| The pain | With Lyra |
|---|---|
| **Twelve worktrees, five agents, and something is holding port 3000.** | **`lyra ps` shows every repo on your laptop: what is running, which ports it holds, and how much memory it uses. `lyra down --all` stops it all.** |
| **Your agent says the tests pass, but you can't see them.** | **You open the same run and its live logs. Either of you can stop it.** |
| **Every new session learns again how to run this client's repo.** | **Your agent saves the commands as tools in the repo. The next session, and the next engineer after you, reuses them.** |
| **Six terminal tabs every morning for the server, the database, and the worker.** | **One screen, one key to start each piece. Closing Lyra stops what it started, and your data stays.** |

**For operations and growth**

| The pain | With Lyra |
|---|---|
| **Every morning, you walk your agent through the same Slack updates and contractor follow-ups.** | **Your agent saves the routine once. Tomorrow, you press `Enter`.** |
| **Every new video means the same export and caption steps, one prompt at a time.** | **The steps become one saved tool. Pick the file and run it.** |

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/ImHangLi/lyra/main/scripts/install.sh | sh
```

Then, in your project:

```sh
lyra setup
```

It installs the Lyra skills for your agent and prints one command to run, such as `claude '…'` or `codex '…'`. Your agent reads the project and saves its real commands as tools. Then run `lyra` to open the workbench.

## Built to be trusted

- **No AI inside.** Lyra runs no models and sends no telemetry. Everything stays on your machine.
- **Fast.** The workbench opens in about 14 ms, and navigation responds in 3 ms (measured on an M5 Pro).
- **Safe by default.** Nothing starts unless you ask. If Lyra crashes, it stops the servers it left behind the next time it starts. Background work ends when its time is up, even across sleep.
- **Works with your agent.** Tested on real repositories with Claude Code, Codex, and a generic agent.

<p align="center"><sub><a href="examples/">Examples</a> · <a href="docs/verification/mvp.md">What we verified</a></sub></p>
