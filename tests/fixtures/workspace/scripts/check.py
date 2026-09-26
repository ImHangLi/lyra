#!/usr/bin/env python3
"""Fixture check: prints a few lines, then exits 0 (or 3 with --fail)."""
import sys, time

for step in ("lint", "types", "unit"):
    print(f"{step}: ok", flush=True)
    time.sleep(0.2)
if "--fail" in sys.argv:
    print("integration: 1 failure", file=sys.stderr, flush=True)
    sys.exit(3)
print("all checks passed", flush=True)
