---
name: lyra-extend
description: Create or change Lyra plugins — save a one-off command as a reusable tool, add a structured table/log panel, or adjust an existing action — then validate and apply without changing the Lyra Core. Use when the user wants a new or modified project tool in `.lyra/`. For running and reading existing tools, use the lyra skill.
---

# Extend Lyra

A plugin is a directory with `plugin.json` and optional scripts. New capability never needs a Core change or rebuild.

## 1. Reuse before creating

```sh
lyra catalog --search "words from the request" --json
lyra describe PLUGIN.ITEM --include-schema --json
```

If a tool already does most of it, change that plugin (new input, new action next to it) instead of adding a near-duplicate. Tell the user which one you reused.

## 2. Choose the runner

| Need | Use |
|---|---|
| Run an existing command; output is just logs | `"run": {"kind": "command", "argv": [...]}` — manifest only, no script |
| Interactive program (prompts, full-screen) | command runner with `"terminal": "pty"` |
| Structured results: tables, trees, grouped logs, row actions | `"run": {"kind": "plugin"}` + plugin `entry`, speaking LPP/1 on stdout |
| Messages published by agents or hooks, nothing to execute | a view-only plugin (no actions) and `lyra publish` |

Pass argv as a list; there is no implicit shell. If you need a shell, write `["/bin/zsh", "-lc", "…"]` and never splice user input into that string — take input from `LYRA_INPUT_FILE`.

Saving a command that worked (`lyra exec … -- ARGV`): turn it into a command action with a clear `title`, a `description` that says what problem it solves, the right `cwd`, and an `input_schema` for the parts that change between uses.

## 3. Write it

Work in a draft: copy `.lyra` to `.lyra/.drafts/<name>/` (without `.drafts`), edit there, and add the plugin path to `workspace.json` `plugins` if it is new. Field reference: [manifest](references/manifest.md). Structured output: [LPP/1 and views](references/protocol.md). Starting points: [command template](templates/command/plugin.json), [structured template](templates/structured/).

Rules that matter:
- stdout of a structured plugin carries only protocol frames; debug output goes to stderr. Exactly one `result` frame, last, for tasks; none for processes.
- Keep secrets out of stdout, views, and manifests. Put required env names in the docs; values come from the user's environment or `env_files`.
- Write private files to `LYRA_STATE_DIR`, rebuildable files to `LYRA_CACHE_DIR`, run outputs to `LYRA_ARTIFACT_DIR`. Never scatter logs in the repo.
- Long-running or polling work must be an explicit `process` or a `schedule` the user enables; saving a plugin never starts it. Give anything that creates external resources (containers, tunnels) a `cleanup` that removes only what it created.
- Prefer a standard view (table with row actions, log, tree) so the human can use the same tool in the TUI. Do not build a second implementation for humans.

## 4. Validate, apply, prove

```sh
lyra validate .lyra/.drafts/<name> --json
lyra apply .lyra/.drafts/<name> --expected-revision N --json   # N = catalog_revision you read
lyra run PLUGIN.ACTION --input sample.json --json    # the normal case
lyra run PLUGIN.ACTION --input bad.json --json       # bad input must fail clearly (SCHEMA_INVALID)
lyra view PLUGIN.VIEW --json                          # structured output is readable
```

- `REVISION_CONFLICT`: someone else applied first. Re-read the catalog, merge onto the current files, apply again. Never overwrite blindly.
- `CONFIG_APPLY_INCOMPLETE`: some files were written. Fix them and run `lyra reload`.
- Check a missing dependency path too (for example the tool is not installed) and make the error readable.
- A small input sample next to the plugin is fine; do not build a test suite for each plugin.

## 5. Report

Return the refs you created or changed, the shortest invocation (`lyra run …`), what you verified, and the limits. Mention that the human sees the same tool in the TUI (`lyra`). Do not paste the whole implementation.
