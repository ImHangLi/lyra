#!/usr/bin/env python3
"""Fixture server stand-in: logs a heartbeat every second until stopped."""
import signal, sys, time

def stop(signum, _frame):
    print(f"received signal {signum}; shutting down", flush=True)
    sys.exit(0)

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
print("dev server ready", flush=True)
n = 0
while True:
    n += 1
    print(f"GET /health 200 ({n})", flush=True)
    time.sleep(1)
