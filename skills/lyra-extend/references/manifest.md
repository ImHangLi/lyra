# Manifest reference (api 1)

Strict JSON: unknown fields, duplicate keys, and `null` for optional fields are errors. IDs match `^[a-z][a-z0-9-]{0,47}$`. `lyra schema plugin` prints the full JSON Schema.

## `.lyra/workspace.json`

```json
{"api": 1, "name": "My Project", "plugins": ["plugins/dev"], "autostart": [], "ui": {"mouse": false}, "meta": {}}
```

`plugins` are paths relative to `.lyra` (or absolute). `autostart` lists process actions started when a human opens a new TUI session; leave it empty unless the user asks.

## `plugin.json`

| Field | Notes |
|---|---|
| `api`, `id`, `name`, `description` | required; the description says what problem the plugin solves |
| `enabled` | default `true`; disabled plugins stay visible but cannot run |
| `tags` | ≤12 short words that help `catalog --search` |
| `entry` | `{"argv": [...]}`; required when any action uses `{"kind":"plugin"}`; runs with cwd = plugin directory |
| `actions`, `views` | at least one of them |
| `config` / `config_schema` | plugin settings (validated); users override them in `.lyra/local.json` |
| `docs` | relative Markdown path inside the plugin, read on demand |

## Action

| Field | Default / rule |
|---|---|
| `id`, `title`, `description`, `mode` | required; `mode` is `task` (ends) or `process` (keeps running) |
| `run` | `{"kind":"command","argv":[...]}` or `{"kind":"plugin"}` |
| `cwd` | `.` = workspace root; relative to the root |
| `env_files`, `env` | layered after the caller's env; `LYRA_*` names are reserved |
| `input_schema` | JSON Schema 2020-12 with an object root; top-level `default`s are filled for CLI and TUI alike; `writeOnly` fields are never echoed |
| `output_schema` | validates a successful structured result's `data` |
| `timeout` | tasks default `{"kind":"after","ms":300000}`, processes `{"kind":"none"}`; never `null` |
| `terminal` | `pipe` (default) or `pty` (command runner only) |
| `stop_signal`, `stop_grace_ms` | `term`/`interrupt`, 100–60000 ms (default 5000), then SIGKILL |
| `cleanup` | `{"kind":"command","argv":[...]}`; runs once after the main process with `LYRA_STOP_REASON` |
| `schedule` | tasks only: `{"every_ms": ≥1000, "params": {...}, "run_on_start": false}`; off until the user enables it |
| `effects` | descriptive tags such as `read-files`, `writes-state`, `network` |

The child receives `LYRA_WORKSPACE_ROOT`, `LYRA_PLUGIN_DIR`, `LYRA_STATE_DIR`, `LYRA_CACHE_DIR`, `LYRA_ARTIFACT_DIR`, `LYRA_RUN_ID`, `LYRA_INPUT_FILE` (effective input JSON), `LYRA_CONFIG_FILE`.

## View

`{"id","title","kind"}` with kind `text|table|log|tree|json`; `persistence` `last` (kept, bounded by retention) or `session`; tables may add `row_actions: [{"action": "show", "bindings": {"path": "path"}}]` mapping input names to column IDs.

## `.lyra/local.json` (personal, not committed)

`{"api": 1, "plugin_patches": {"dev": {"actions": [...]}}}` — RFC 7396 merge patches per plugin ID (arrays replace; `null` deletes). Never edit another person's local file.
