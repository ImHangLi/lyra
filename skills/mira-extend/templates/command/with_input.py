"""Run a command whose arguments need logic on the action input.

For a plain value in an argument, use a placeholder in the manifest instead:
"argv": ["npm", "run", "dev", "--", "--port", "{input.port}"]. Use a script like this
when the arguments need code: optional flags, lists, or values computed from the input.
The input is JSON in MIRA_INPUT_FILE. The arguments stay a list, so nothing is parsed
by a shell.
"""

import json
import os

with open(os.environ["MIRA_INPUT_FILE"], encoding="utf-8") as f:
    params = json.load(f)

argv = ["npm", "run", "dev", "--", "--port", str(params.get("port", 3000))]
if params.get("host"):
    argv += ["--host", params["host"]]
os.execvp(argv[0], argv)
