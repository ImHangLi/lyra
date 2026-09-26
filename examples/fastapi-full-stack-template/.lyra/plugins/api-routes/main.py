#!/usr/bin/env python3
"""LPP/1 plugin: list FastAPI endpoints per route file by reading the source with ast."""
import ast
import json
import sys
from pathlib import Path

ROUTES_DIR = "backend/app/api/routes"
METHODS = ("get", "post", "put", "patch", "delete", "options", "head")


def emit(frame):
    print(json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False), flush=True)


def fail(code, summary, message):
    emit({"type": "result", "ok": False, "summary": summary, "data": None,
          "error": {"code": code, "message": message, "retryable": False}})
    return 1


def const_str(node):
    return node.value if isinstance(node, ast.Constant) and isinstance(node.value, str) else None


def router_prefixes(tree):
    """Map each `name = APIRouter(prefix=...)` variable to its prefix."""
    prefixes = {}
    for node in ast.walk(tree):
        if (isinstance(node, ast.Assign) and isinstance(node.value, ast.Call)
                and getattr(node.value.func, "id", None) == "APIRouter"):
            prefix = next((const_str(k.value) or "" for k in node.value.keywords if k.arg == "prefix"), "")
            for target in node.targets:
                if isinstance(target, ast.Name):
                    prefixes[target.id] = prefix
    return prefixes


def endpoints(path):
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    prefixes = router_prefixes(tree)
    for node in tree.body:
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        for dec in node.decorator_list:
            if not (isinstance(dec, ast.Call) and isinstance(dec.func, ast.Attribute)
                    and dec.func.attr in METHODS and isinstance(dec.func.value, ast.Name)
                    and dec.func.value.id in prefixes):
                continue
            route = const_str(dec.args[0]) if dec.args else None
            if route is None:
                route = next((const_str(k.value) for k in dec.keywords if k.arg == "path"), None)
            yield {"method": dec.func.attr.upper(), "route": prefixes[dec.func.value.id] + (route or "?"),
                   "function": node.name, "line": dec.lineno}


def main():
    try:
        req = json.loads(sys.stdin.readline())
        api_prefix = req["input"].get("api_prefix", "/api/v1").rstrip("/")
        only = req["input"].get("file", "")
        routes_dir = Path(req["context"]["workspace_root"]) / ROUTES_DIR
        if not routes_dir.is_dir():
            return fail("NO_ROUTES_DIR", "Route directory not found.", f"{routes_dir} does not exist.")
        files = sorted(p for p in routes_dir.glob("*.py") if not only or p.name == only)
        if not files:
            return fail("NO_ROUTE_FILES", "No matching route files.",
                        f"No {only or '*.py'} in {ROUTES_DIR}.")
        rows = []
        for path in files:
            found = list(endpoints(path))
            if found:
                emit({"type": "log", "level": "info", "text": f"{path.name}: {len(found)} endpoint(s)"})
            for ep in found:
                rows.append({"id": f"{path.name}:{ep['line']}:{ep['method']}", "values": {
                    "file": path.name, "method": ep["method"], "path": api_prefix + ep["route"],
                    "function": ep["function"], "line": ep["line"]}})
        emit({"type": "view", "view_id": "endpoints", "op": "replace", "data": {
            "kind": "table",
            "columns": [
                {"id": "file", "label": "Route file", "type": "text"},
                {"id": "method", "label": "Method", "type": "text"},
                {"id": "path", "label": "Path", "type": "text"},
                {"id": "function", "label": "Handler", "type": "text"},
                {"id": "line", "label": "Line", "type": "number"},
            ],
            "rows": rows,
        }})
        groups = sorted({r["values"]["file"] for r in rows})
        emit({"type": "result", "ok": True,
              "summary": f"{len(rows)} endpoint(s) in {len(groups)} route file(s).",
              "data": {"endpoints": len(rows), "files": groups}})
        return 0
    except (KeyError, ValueError, TypeError, OSError, SyntaxError) as exc:
        print(f"list failed: {exc}", file=sys.stderr)
        return fail("SCAN_FAILED", "Could not read the route files.", str(exc))


if __name__ == "__main__":
    raise SystemExit(main())
