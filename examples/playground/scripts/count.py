#!/usr/bin/env python3
"""Increments a counter in LYRA_STATE_DIR so repeated side effects are visible."""
import os, pathlib

state = pathlib.Path(os.environ.get("LYRA_STATE_DIR", "."))
state.mkdir(parents=True, exist_ok=True)
counter = state / "count.txt"
value = int(counter.read_text()) + 1 if counter.exists() else 1
counter.write_text(str(value))
print(f"count is now {value}")
