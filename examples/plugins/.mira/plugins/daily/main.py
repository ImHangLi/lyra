"""MPP/1 task plugin: draft a daily update from git and TODO markers, without network."""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

SKIP = {".git", ".mira", "node_modules", "target", "__pycache__", ".venv"}
MAX_FILES = 5000
MAX_FILE_BYTES = 1_000_000


def emit(frame):
    line = json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False)
    print(line, flush=True)


def commits(root):
    argv = ["git", "log", "--since=1 day ago", "--no-merges", "--pretty=format:- %s (%an)"]
    try:
        out = subprocess.run(argv, cwd=root, capture_output=True, text=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return None, str(exc)
    if out.returncode != 0:
        return None, out.stderr.strip() or "git log failed"
    return [line for line in out.stdout.splitlines() if line.strip()], None


def todos(root):
    count, files, seen = 0, 0, 0
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP)
        for name in filenames:
            if seen >= MAX_FILES:
                return count, files
            path = Path(dirpath) / name
            try:
                if path.stat().st_size > MAX_FILE_BYTES:
                    continue
                text = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            seen += 1
            n = text.count("TODO")
            if n:
                count, files = count + n, files + 1
    return count, files


def main():
    try:
        req = json.loads(sys.stdin.readline())
        ctx = req["context"]
        root = Path(ctx["workspace_root"])
        today = time.strftime("%Y-%m-%d")
        lines, git_error = commits(root)
        todo_count, todo_files = todos(root)
        parts = [f"# Daily update, {today}", "", "## Changes in the last day"]
        if git_error:
            parts.append(f"Could not read git: {git_error}")
        else:
            parts.extend(lines or ["No commits."])
        parts += ["", "## Open TODOs", f"{todo_count} TODO marker(s) in {todo_files} file(s).", ""]
        text = "\n".join(parts)
        path = Path(ctx["artifact_dir"]) / f"daily-{today}.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        data = {"kind": "text", "text": text, "format": "markdown"}
        emit({"type": "view", "view_id": "update", "op": "replace", "data": data})
        emit({
            "type": "artifact", "path": str(path), "mime": "text/markdown",
            "label": f"Daily update {today}", "ownership": "managed",
        })
        commit_count = 0 if git_error else len(lines)
        summary = f"{commit_count} commit(s), {todo_count} TODO marker(s)."
        result = {
            "type": "result", "ok": True, "summary": summary,
            "data": {"commits": commit_count, "todos": todo_count},
        }
        emit(result)
        return 0
    except (KeyError, ValueError, TypeError, OSError) as exc:
        print(f"draft failed: {exc}", file=sys.stderr)
        error = {"code": "DRAFT_FAILED", "message": str(exc), "retryable": False}
        result = {
            "type": "result", "ok": False, "summary": "Could not draft the update.",
            "data": None, "error": error,
        }
        emit(result)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
