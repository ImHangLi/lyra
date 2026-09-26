#!/usr/bin/env python3
"""Reports environment layering; secret values are only checked for presence."""
import os

print("LAYER =", os.environ.get("LAYER"))
print("FROM_FILE =", os.environ.get("FROM_FILE"))
print("CLIENT_ONLY =", os.environ.get("CLIENT_ONLY"))
print("API_TOKEN present:", "API_TOKEN" in os.environ)
print("MIRA_RUN_ID =", os.environ.get("MIRA_RUN_ID"))
print("input file exists:", os.path.exists(os.environ.get("MIRA_INPUT_FILE", "")))
