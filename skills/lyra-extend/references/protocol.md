# LPP/1 and views

A structured plugin reads one JSON line (the invocation) from stdin, then writes one JSON object per line to stdout. stdout is the protocol; stderr is free-form diagnostics.

## Invocation (stdin)

`{"api":1,"run_id":"r_…","action":"scan","input":{...},"config":{...},"context":{"workspace_id","workspace_root","cwd","plugin_dir","state_dir","cache_dir","artifact_dir"}}`. Run project commands with `context.cwd` explicitly; the process cwd is the plugin directory.

## Frames (stdout)

Every frame has `"api": 1` and `"type"`. At most 1 MiB per line.

| type | fields |
|---|---|
| `log` | `level` (debug/info/warn/error), `text` ≤ 8 KiB, optional `fields` |
| `progress` | `message`, optional `current` and `total` together |
| `status` | `state` (healthy/warn/error/unknown), `message` — reported health, not the run's lifecycle |
| `view` | `view_id`, `op` (`replace`, or `append` for log views), `data` |
| `artifact` | `path`, `mime`, `label`, `ownership` (`managed` = inside `artifact_dir`; `external` = reference only) |
| `result` | `ok`, `summary`, `data` (may be `null`), and `error` `{code, message, retryable}` exactly when `ok` is false |

A task must end with exactly one `result`. A success result followed by a non-zero exit is a failure. Process actions never emit `result`.

## View data

| kind | data |
|---|---|
| `text` | `{"kind":"text","text":"…","format":"plain"\|"markdown"}` |
| `table` | `{"kind":"table","columns":[{"id","label","type":"text\|number\|boolean\|timestamp"}],"rows":[{"id","values":{colId: value\|null}}]}` — every row has every column; unique IDs |
| `log` | `{"kind":"log","items":[{"id","text","level"}]}` — reuse an ID only for identical content |
| `tree` | `{"kind":"tree","nodes":[{"id","label","children":[...]}]}` |
| `json` | `{"kind":"json","value":…}` |

An invalid update is rejected whole and the previous data stays (marked stale). Timestamps are UTC like `2026-09-26T01:34:00.000Z`. Numbers must be finite; large integers go in strings.

## Minimal Python pattern

```python
import json, sys

req = json.loads(sys.stdin.readline())


def emit(frame):
    print(json.dumps({"api": 1, **frame}), flush=True)


rows = [{"id": "1", "values": {"name": "example", "count": 3}}]
emit({
    "type": "view",
    "view_id": "items",
    "op": "replace",
    "data": {
        "kind": "table",
        "columns": [
            {"id": "name", "label": "Name", "type": "text"},
            {"id": "count", "label": "Count", "type": "number"},
        ],
        "rows": rows,
    },
})
emit({
    "type": "result",
    "ok": True,
    "summary": f"{len(rows)} item(s)",
    "data": {"items": len(rows)},
})
```
