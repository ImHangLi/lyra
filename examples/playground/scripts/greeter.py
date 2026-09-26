#!/usr/bin/env python3
"""Long-running process that logs the greeting from its Lyra input file."""
import json, os, time

greeting = json.load(open(os.environ["LYRA_INPUT_FILE"])).get("greeting", "hello")
while True:
    print(f"{greeting} from pid {os.getpid()}", flush=True)
    time.sleep(1)
