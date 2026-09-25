#!/usr/bin/env python3
"""LPP/1 plugin template: one invocation line in, protocol frames out, diagnostics on stderr."""
import json
import sys


def emit(frame):
    print(json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False), flush=True)


def main():
    try:
        req = json.loads(sys.stdin.readline())
        limit = int(req["input"].get("limit", 100))
        rows = [{"id": str(i), "values": {"name": f"item {i}", "count": i}} for i in range(1, min(limit, 3) + 1)]
        emit({"type": "view", "view_id": "items", "op": "replace", "data": {
            "kind": "table",
            "columns": [{"id": "name", "label": "Name", "type": "text"},
                        {"id": "count", "label": "Count", "type": "number"}],
            "rows": rows,
        }})
        emit({"type": "result", "ok": True, "summary": f"{len(rows)} item(s).", "data": {"items": len(rows)}})
        return 0
    except (KeyError, ValueError, TypeError) as exc:
        print(f"refresh failed: {exc}", file=sys.stderr)
        emit({"type": "result", "ok": False, "summary": "Could not refresh.", "data": None,
              "error": {"code": "REFRESH_FAILED", "message": str(exc), "retryable": False}})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
