#!/usr/bin/env python3
"""LPP/1 fault fixture: writes valid, split, or broken stdout chosen by `input.case`."""
from __future__ import annotations

import json
import os
import random
import sys
import time
from pathlib import Path
from typing import Any

OUT = sys.stdout.buffer


def line(frame: dict[str, Any]) -> bytes:
    return (json.dumps({"api": 1, **frame}, ensure_ascii=False) + "\n").encode("utf-8")


def write(data: bytes) -> None:
    OUT.write(data)
    OUT.flush()


def result(case: str, ok: bool = True) -> bytes:
    if ok:
        return line({"type": "result", "ok": True, "summary": f"case {case} done", "data": {"case": case}})
    return line({"type": "result", "ok": False, "summary": f"case {case} failed on purpose", "data": None,
                 "error": {"code": "CASE_FAILED", "message": "requested failure", "retryable": False}})


def note(text: str) -> bytes:
    return line({"type": "view", "view_id": "note", "op": "replace", "data": {"kind": "text", "text": text}})


COLUMNS = [
    {"id": "name", "label": "Name", "type": "text"},
    {"id": "size", "label": "Size", "type": "number"},
    {"id": "ok", "label": "OK", "type": "boolean"},
]


def table(rows: list[dict[str, Any]]) -> bytes:
    return line({"type": "view", "view_id": "grid", "op": "replace",
                 "data": {"kind": "table", "columns": COLUMNS, "rows": rows}})


def log_items(items: list[dict[str, Any]]) -> bytes:
    return line({"type": "view", "view_id": "feed", "op": "append", "data": {"kind": "log", "items": items}})


def artifact(path: str, ownership: str) -> bytes:
    return line({"type": "artifact", "path": path, "mime": "text/plain", "label": f"{ownership} sample",
                 "ownership": ownership})


def emit(case: str, ctx: dict[str, Any]) -> int:
    text = "UTF-8 split check: héllo ✓ 日本語 🎉"
    good = note(text) + line({"type": "log", "level": "info", "text": text, "fields": {"case": case}}) + result(case)
    if case == "byte-split":
        for b in good:
            write(bytes([b]))
            time.sleep(0.001)
    elif case == "chunk-split":
        rng = random.Random(7)
        i = 0
        while i < len(good):
            n = rng.randint(1, 9)
            write(good[i:i + n])
            i += n
            time.sleep(0.002)
    elif case == "many-per-chunk":
        batch = b"".join(line({"type": "log", "level": "debug", "text": f"frame {n}"}) for n in range(50))
        write(batch + good)
    elif case == "crlf":
        write(good.replace(b"\n", b"\r\n"))
    elif case == "status-progress":
        write(line({"type": "status", "state": "healthy", "message": "all good"}))
        for n in range(1, 4):
            write(line({"type": "progress", "message": f"step {n}", "current": n, "total": 3}))
            time.sleep(0.3)
        write(result(case))
    elif case == "invalid-json":
        write(note("before the bad frame") + b'{"api":1,"type":"log",\n' + result(case))
    elif case == "half-frame":
        write(b'{"api":1,"type":"log","level":"info","te')
        time.sleep(40)
    elif case == "truncated-eof":
        write(result(case).rstrip(b"\n"))
    elif case == "oversized":
        chunk = b"x" * 65536
        write(b'{"api":1,"type":"log","level":"info","text":"')
        for _ in range(20):
            write(chunk)
        write(b'"}\n')
    elif case == "bom":
        write(b"\xef\xbb\xbf" + result(case))
    elif case == "empty-line":
        write(b"\n" + result(case))
    elif case == "duplicate-result":
        write(result(case) + result(case))
    elif case == "frame-after-result":
        write(result(case) + line({"type": "log", "level": "info", "text": "too late"}))
    elif case == "no-result":
        write(note("no result follows"))
    elif case == "exit1-after-success":
        write(result(case))
        return 1
    elif case == "failure-result":
        write(result(case, ok=False))
    elif case == "bad-output":
        write(line({"type": "result", "ok": True, "summary": "wrong shape", "data": {"unexpected": 1}}))
    elif case == "good-table":
        write(table([{"id": "a", "values": {"name": "alpha", "size": 1, "ok": True}},
                     {"id": "b", "values": {"name": "beta", "size": 2.5, "ok": None}}]) + result(case))
    elif case == "bad-table-type":
        write(table([{"id": "a", "values": {"name": "alpha", "size": "one", "ok": True}}]) + result(case))
    elif case == "bad-table-missing-column":
        write(table([{"id": "a", "values": {"name": "alpha", "size": 1}}]) + result(case))
    elif case == "bad-table-duplicate-row":
        write(table([{"id": "a", "values": {"name": "alpha", "size": 1, "ok": True}},
                     {"id": "a", "values": {"name": "again", "size": 2, "ok": False}}]) + result(case))
    elif case == "log-a":
        write(log_items([{"id": "a", "text": "first"}, {"id": "b", "text": "second"}]) + result(case))
    elif case == "log-a-again":
        write(log_items([{"id": "a", "text": "first"}]) + result(case))
    elif case == "log-a-changed":
        write(log_items([{"id": "a", "text": "different"}]) + result(case))
    elif case == "artifact-managed":
        path = Path(ctx["artifact_dir"]) / "report.txt"
        path.write_text("managed report\nline two ✓\n", encoding="utf-8")
        write(artifact(str(path), "managed") + result(case))
    elif case == "artifact-escape":
        write(artifact("README.md", "managed") + result(case))
    elif case == "artifact-symlink-escape":
        link = Path(ctx["artifact_dir"]) / "escape.txt"
        link.symlink_to(Path(ctx["workspace_root"]) / "README.md")
        write(artifact(str(link), "managed") + result(case))
    elif case == "artifact-external":
        write(artifact("README.md", "external") + artifact("missing-file.txt", "external") + result(case))
    return 0


def main() -> int:
    request = json.loads(sys.stdin.buffer.readline(1_048_578).decode("utf-8"))
    print(f"bad-frames: action={request['action']}", file=sys.stderr, flush=True)
    if request["action"] == "emit":
        return emit(request["input"]["case"], request["context"])
    if request["action"] == "slow":
        time.sleep(5)
        write(result("slow"))
        return 0
    if request["action"] == "proc":
        write(line({"type": "status", "state": "healthy", "message": "started"}))
        write(result("proc"))
        time.sleep(30)
        return 0
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
