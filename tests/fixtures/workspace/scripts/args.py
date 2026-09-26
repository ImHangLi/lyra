#!/usr/bin/env python3
"""Print the arguments this process received, one per line, to show argv templates."""
import sys

for i, arg in enumerate(sys.argv[1:], 1):
    print(f"argv[{i}] = {arg!r}", flush=True)
