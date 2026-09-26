"""MPP/1 service plugin: follow another run's logs and collect the lines that match."""

import json
import os
import subprocess
import sys
import time

RETRY_SECONDS = 5


def emit(frame):
    line = json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False)
    print(line, flush=True)


def status(state, message):
    emit({"type": "status", "state": state, "message": message})


def follow(root, target, pattern):
    """Runs `mira logs --follow` once; returns when the followed run ends."""
    argv = [
        os.environ["MIRA_BIN"], "--project", root, "logs", target,
        "--follow", "--json", "--grep", pattern,
    ]
    with subprocess.Popen(argv, stdout=subprocess.PIPE, text=True) as child:
        for line in child.stdout:
            frame = json.loads(line)
            if frame.get("ok") is False:
                # A reply instead of a stream, for example when the target never ran.
                status("warn", frame["error"]["message"])
                continue
            if frame.get("type") == "ready":
                status("healthy", f"following {target}")
            if frame.get("type") != "log":
                continue
            run_id = frame["data"]["run_id"]
            items = [
                {"id": f"{run_id}-{r['log_seq']}", "text": r["text"][:2000], "level": "error"}
                for r in frame["data"]["records"]
            ]
            view = {"kind": "log", "items": items}
            emit({"type": "view", "view_id": "lines", "op": "append", "data": view})


def main():
    req = json.loads(sys.stdin.readline())
    root = req["context"]["workspace_root"]
    target = req["input"].get("target", "dev.web")
    pattern = req["input"].get("pattern", "error")
    while True:
        try:
            follow(root, target, pattern)
        except (OSError, ValueError, KeyError) as exc:
            print(f"follow failed: {exc}", file=sys.stderr)
        status("unknown", f"{target} is not running; trying again in {RETRY_SECONDS}s")
        time.sleep(RETRY_SECONDS)


if __name__ == "__main__":
    raise SystemExit(main())
