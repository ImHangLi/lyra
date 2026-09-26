#!/usr/bin/env python3
"""Prints about LINES_PER_SECOND lines of ~200 bytes each until stopped."""
import os, sys, time

rate = int(os.environ.get("LINES_PER_SECOND", "5000"))
pad = "x" * 170
n = 0
start = time.monotonic()
while True:
    n += 1
    sys.stdout.write(f"line {n:09d} {pad}\n")
    if n % 100 == 0:
        sys.stdout.flush()
        ahead = n / rate - (time.monotonic() - start)
        if ahead > 0:
            time.sleep(ahead)
