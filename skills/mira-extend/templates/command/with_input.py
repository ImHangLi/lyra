"""Run a command whose arguments come from the action input.

Command actions do not template argv: input reaches the child only as JSON in
MIRA_INPUT_FILE. Point the action at this script and map input fields to
arguments here. The arguments stay a list, so nothing is parsed by a shell.
"""

import json
import os

with open(os.environ["MIRA_INPUT_FILE"], encoding="utf-8") as f:
    params = json.load(f)

argv = ["npm", "run", "dev", "--", "--port", str(params.get("port", 3000))]
os.execvp(argv[0], argv)
