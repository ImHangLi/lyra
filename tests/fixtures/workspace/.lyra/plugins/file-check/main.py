#!/usr/bin/env python3
"""LPP/1 example. No AI or third-party Python dependency."""
from __future__ import annotations

import json
import stat
import sys
from pathlib import Path
from typing import Any

MAX_FRAME = 1_048_576
MAX_SAFE_INTEGER = (1 << 53) - 1


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON object key")
        result[key] = value
    return result


def reject_constant(value: str) -> None:
    raise ValueError(f"Unsupported JSON numeric constant: {value}")


def emit(frame: dict[str, Any]) -> None:
    line = json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False)
    if len(line.encode("utf-8")) > MAX_FRAME:
        raise ValueError("Output exceeds the LPP frame budget")
    print(line, flush=True)


def main() -> int:
    try:
        raw = sys.stdin.buffer.readline(MAX_FRAME + 2)
        if not raw.endswith(b"\n") or len(raw.rstrip(b"\r\n")) > MAX_FRAME:
            raise ValueError("Expected one complete bounded JSONL invocation")
        request = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=unique_object,
            parse_constant=reject_constant,
        )
        if not isinstance(request, dict):
            raise ValueError("Invocation must be an object")
        if type(request.get("api")) is not int or request["api"] != 1:
            raise ValueError("Unsupported api")
        if request.get("action") != "inspect":
            raise ValueError("Unsupported action")
        payload = request["input"]
        if not isinstance(payload, dict) or set(payload) != {"paths"}:
            raise ValueError("Input must contain exactly paths")
        paths = payload["paths"]
        if not isinstance(paths, list) or not 1 <= len(paths) <= 50:
            raise ValueError("paths must contain 1 to 50 names")
        cwd_value = request["context"]["cwd"]
        if not isinstance(cwd_value, str) or not Path(cwd_value).is_absolute():
            raise ValueError("context.cwd must be absolute")
        cwd = Path(cwd_value)
        rows: list[dict[str, Any]] = []
        present = 0
        for index, name in enumerate(paths):
            if not isinstance(name, str) or not 1 <= len(name) <= 4096 or "\x00" in name:
                raise ValueError("Each path must be a nonempty file name")
            # Only files inside the workspace: absolute paths and `..` escapes are rejected, so
            # the tool never reports on files elsewhere on the machine.
            path = (cwd / name).resolve()
            if Path(name).is_absolute() or not path.is_relative_to(cwd.resolve()):
                raise ValueError(f"{name!r} is outside the workspace")
            try:
                info = path.stat()
            except FileNotFoundError:
                info = None
            exists = info is not None and stat.S_ISREG(info.st_mode)
            size = info.st_size if exists and info is not None else None
            if size is not None and not 0 <= size <= MAX_SAFE_INTEGER:
                raise ValueError("File size is outside the numeric protocol range")
            present += int(exists)
            rows.append({"id": str(index), "values": {"path": name, "present": exists, "bytes": size}})
        emit({
            "type": "view", "view_id": "files", "op": "replace",
            "data": {
                "kind": "table",
                "columns": [
                    {"id": "path", "label": "Path", "type": "text"},
                    {"id": "present", "label": "Present", "type": "boolean"},
                    {"id": "bytes", "label": "Bytes", "type": "number"},
                ],
                "rows": rows,
            },
        })
        emit({"type": "result", "ok": True, "summary": f"{present}/{len(paths)} files are present.",
              "data": {"checked": len(paths), "present": present}})
        return 0
    except (KeyError, TypeError, ValueError, OSError, UnicodeError) as exc:
        emit({"type": "result", "ok": False, "summary": "Could not inspect selected files.",
              "data": None, "error": {"code": "INSPECT_FAILED", "message": str(exc), "retryable": False}})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
