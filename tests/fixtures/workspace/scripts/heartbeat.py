#!/usr/bin/env python3
"""Scheduled fixture task: counts TODO markers in README.md and appends to a state file."""
import os, pathlib, time

root = pathlib.Path(os.environ["LYRA_WORKSPACE_ROOT"])
count = root.joinpath("README.md").read_text().count("TODO")
state = pathlib.Path(os.environ["LYRA_STATE_DIR"])
state.mkdir(parents=True, exist_ok=True)
with open(state / "heartbeat.log", "a") as f:
    f.write(f"{time.strftime('%H:%M:%S')} todo={count}\n")
print(f"TODO markers in README.md: {count}")
if os.environ.get("HEARTBEAT_SLOW"):
    time.sleep(float(os.environ["HEARTBEAT_SLOW"]))
