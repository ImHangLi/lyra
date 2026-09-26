#!/usr/bin/env python3
"""LPP/1 marker scanner plugin using only the Python standard library."""
from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path
from typing import Any

SKIP = {".git", ".lyra", "node_modules", "target", "__pycache__", ".venv"}


def emit(frame: dict[str, Any]) -> None:
    print(json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False), flush=True)


def scan(root: Path, marker: str, max_files: int) -> int:
    rows, tree, files = [], {}, 0
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP)
        for name in sorted(filenames):
            if files >= max_files:
                break
            path = Path(dirpath) / name
            try:
                text = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            files += 1
            rel = str(path.relative_to(root))
            for number, line in enumerate(text.splitlines(), 1):
                if marker in line:
                    rows.append({"id": f"{rel}:{number}", "values": {"path": rel, "line": number, "text": line.strip()[:200]}})
                    tree.setdefault(str(Path(rel).parent), []).append(f"{rel}:{number}")
    emit({"type": "view", "view_id": "table", "op": "replace", "data": {
        "kind": "table",
        "columns": [
            {"id": "path", "label": "File", "type": "text"},
            {"id": "line", "label": "Line", "type": "number"},
            {"id": "text", "label": "Text", "type": "text"},
        ],
        "rows": rows,
    }})
    nodes = [{"id": f"dir:{d}", "label": f"{d} ({len(items)})", "children": [{"id": i, "label": i} for i in items]}
             for d, items in sorted(tree.items())]
    emit({"type": "view", "view_id": "tree", "op": "replace", "data": {"kind": "tree", "nodes": nodes}})
    summary = f"{len(rows)} `{marker}` match(es) in {files} file(s)."
    emit({"type": "view", "view_id": "summary", "op": "replace", "data": {"kind": "text", "text": summary, "format": "markdown"}})
    emit({"type": "view", "view_id": "raw", "op": "replace", "data": {"kind": "json", "value": {"marker": marker, "files": files, "matches": len(rows)}}})
    stamp = time.strftime("%Y%m%dT%H%M%S")
    emit({"type": "view", "view_id": "events", "op": "append", "data": {"kind": "log", "items": [
        {"id": f"scan-{stamp}-{os.getpid()}", "text": summary, "level": "info"}]}})
    emit({"type": "result", "ok": True, "summary": summary, "data": {"files": files, "matches": len(rows)}})
    return 0


def show(root: Path, rel: str, line: int) -> int:
    path = (root / rel).resolve()
    if root not in path.parents:
        raise ValueError("path must stay inside the workspace")
    lines = path.read_text(encoding="utf-8").splitlines()
    lo, hi = max(1, line - 3), min(len(lines), line + 3)
    body = "\n".join(f"{n:5d}{'>' if n == line else ' '} {lines[n - 1]}" for n in range(lo, hi + 1))
    emit({"type": "view", "view_id": "context", "op": "replace", "data": {"kind": "text", "text": f"{rel}\n\n{body}"}})
    emit({"type": "result", "ok": True, "summary": f"Showed {rel}:{line}.", "data": None})
    return 0


def main() -> int:
    try:
        request = json.loads(sys.stdin.buffer.readline(1_048_578).decode("utf-8"))
        root = Path(request["context"]["workspace_root"]).resolve()
        payload = request["input"]
        if request["action"] == "scan":
            return scan(root, payload.get("marker", "TODO"), int(payload.get("max_files", 200)))
        if request["action"] == "show":
            return show(root, payload["path"], int(payload["line"]))
        raise ValueError(f"unsupported action {request['action']!r}")
    except (KeyError, TypeError, ValueError, OSError) as exc:
        emit({"type": "result", "ok": False, "summary": "The scan failed.", "data": None,
              "error": {"code": "SCAN_FAILED", "message": str(exc), "retryable": False}})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
