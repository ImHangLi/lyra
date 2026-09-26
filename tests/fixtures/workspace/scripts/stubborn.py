#!/usr/bin/env python3
"""Ignores SIGTERM and SIGINT; only SIGKILL stops it."""
import signal, time

signal.signal(signal.SIGTERM, lambda *_: print("ignoring SIGTERM", flush=True))
signal.signal(signal.SIGINT, lambda *_: print("ignoring SIGINT", flush=True))
print("stubborn process ready", flush=True)
while True:
    time.sleep(0.2)
