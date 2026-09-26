<p align="center">
  <img src=".github/assets/lyra-hero.png" alt="Lyra" width="100%">
</p>

<h1 align="center">Lyra</h1>

<h3 align="center">You and your agent, on the same page.</h3>

<p align="center">
  <a href="https://github.com/ImHangLi/lyra/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/ImHangLi/lyra?style=flat-square&color=f26b3a"></a>
  <img alt="Platform" src="https://img.shields.io/badge/macOS-arm64-f26b3a?style=flat-square">
</p>

---

Your agents do the work. You still have to find it, check it, and explain it again tomorrow.

**Lyra gives you and your agents one shared place to run your work, and it remembers what worked.**

<p align="center"><img src=".github/assets/lyra-architecture.png" alt="How Lyra works: the human uses the TUI and agents use the CLI. Both talk to one Lyra host per workspace over LIPC/1 (JSON-RPC 2.0 on a Unix socket) and receive stream events. The host owns runs (pipe or PTY), plugins (LPP/1 JSONL), a SQLite ledger in WAL mode, and typed views (table, tree, log, text, JSON)." width="100%"></p>

## What it fixes

| The pain | With Lyra |
|---|---|
| **You can't tell which terminal tab is the dev server.** | **One screen shows every run, its status, and its logs.** |
| **Your agent says it worked, but you can't see it.** | **You and your agent watch the same run, and either of you can stop it.** |
| **You explain last week's fix to your agent again.** | **The command that worked is saved, and the next agent finds it.** |
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

**No AI inside.** Lyra runs no models and sends no telemetry. Everything stays on your machine.

<p align="center"><sub><a href="examples/">Examples</a> · <a href="docs/verification/mvp.md">What we verified</a></sub></p>
