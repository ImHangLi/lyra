"""MPP/1 plugin template: one invocation line in, frames out, diagnostics on stderr."""

import json
import sys


def emit(frame):
    line = json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False)
    print(line, flush=True)


def table(rows):
    return {
        "kind": "table",
        "columns": [
            {"id": "name", "label": "Name", "type": "text"},
            {"id": "count", "label": "Count", "type": "number"},
        ],
        "rows": rows,
    }


def main():
    try:
        req = json.loads(sys.stdin.readline())
        limit = int(req["input"].get("limit", 100))
        rows = [
            {"id": str(i), "values": {"name": f"item {i}", "count": i}}
            for i in range(1, min(limit, 3) + 1)
        ]
        emit({"type": "view", "view_id": "items", "op": "replace", "data": table(rows)})
        result = {
            "type": "result",
            "ok": True,
            "summary": f"{len(rows)} item(s).",
            "data": {"items": len(rows)},
        }
        emit(result)
        return 0
    except (KeyError, ValueError, TypeError) as exc:
        print(f"refresh failed: {exc}", file=sys.stderr)
        error = {"code": "REFRESH_FAILED", "message": str(exc), "retryable": False}
        result = {
            "type": "result",
            "ok": False,
            "summary": "Could not refresh.",
            "data": None,
            "error": error,
        }
        emit(result)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
