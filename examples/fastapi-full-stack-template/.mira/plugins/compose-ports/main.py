#!/usr/bin/env python3
"""MPP/1 plugin: list Docker Compose services and published ports without Docker.

PyYAML is not guaranteed on the host, so this reads the block-style subset of YAML
that Compose files use: `services:` at the top level, service names one level in,
and `ports:` as a block or flow list of short-syntax strings or long-syntax maps.
"""
import json
import re
import sys
from pathlib import Path

PATTERNS = ("compose*.yml", "compose*.yaml", "docker-compose*.yml", "docker-compose*.yaml")


def emit(frame):
    print(json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False), flush=True)


def strip_comment(line):
    quote = None
    for i, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch == "#" and (i == 0 or line[i - 1] in " \t"):
            return line[:i]
    return line


def unquote(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def logical_lines(text):
    for number, raw in enumerate(text.splitlines(), 1):
        line = strip_comment(raw).rstrip()
        if line.strip():
            yield number, len(line) - len(line.lstrip(" ")), line.strip()


def parse_short(spec):
    """Short syntax: [HOST_IP:][PUBLISHED:]TARGET[/PROTOCOL]."""
    spec, _, protocol = spec.partition("/")
    host_ip = None
    m = re.match(r"^\[([^\]]+)\]:(.*)$", spec)  # [::1]:8080:80
    if m:
        host_ip, spec = m.group(1), m.group(2)
    parts = spec.split(":")
    if host_ip is None and len(parts) == 3:
        host_ip = parts.pop(0)
    published = parts[0] if len(parts) == 2 else None
    return {"published": published or None, "target": parts[-1], "protocol": protocol or "tcp", "host_ip": host_ip or None}


def parse_long(fields):
    return {
        "published": fields.get("published") or None,
        "target": fields.get("target"),
        "protocol": fields.get("protocol") or "tcp",
        "host_ip": fields.get("host_ip") or None,
    }


def parse_compose(text):
    """Return {service: [port dicts]} in file order."""
    services = {}
    lines = list(logical_lines(text))
    i, n = 0, len(lines)
    in_services = False
    service_indent = key_indent = None
    current = None
    while i < n:
        _, indent, content = lines[i]
        if indent == 0:
            in_services = content == "services:"
            service_indent = key_indent = current = None
            i += 1
            continue
        if not in_services:
            i += 1
            continue
        if service_indent is None:
            service_indent = indent
        if indent == service_indent and content.endswith(":"):
            current = unquote(content[:-1])
            services.setdefault(current, [])
            key_indent = None
            i += 1
            continue
        if current is None or indent <= service_indent:
            i += 1
            continue
        if key_indent is None:
            key_indent = indent
        key, sep, rest = content.partition(":")
        if indent != key_indent or not sep or key.strip() != "ports":
            i += 1
            continue
        rest = rest.strip()
        i += 1
        if rest.startswith("["):
            items = [unquote(x) for x in rest.strip("[]").split(",") if x.strip()]
            services[current].extend(parse_short(x) for x in items)
            continue
        # Block list: items deeper than the `ports` key.
        while i < n and lines[i][1] > key_indent:
            _, item_indent, item = lines[i]
            i += 1
            if not item.startswith("-"):
                continue
            item = item[1:].strip()
            k, s, v = item.partition(":")
            if s and re.match(r"^[a-z_]+$", k) and (v == "" or v.startswith(" ")):
                fields = {k: unquote(v)}
                while i < n and lines[i][1] > item_indent:
                    fk, _, fv = lines[i][2].partition(":")
                    fields[fk.strip()] = unquote(fv)
                    i += 1
                services[current].append(parse_long(fields))
            else:
                services[current].append(parse_short(unquote(item)))
    return services


def main():
    try:
        req = json.loads(sys.stdin.readline())
        only_published = bool(req["input"].get("only_published", False))
        root = Path(req["context"]["workspace_root"])
        files = sorted({p for pattern in PATTERNS for p in root.glob(pattern) if p.is_file()})
        if not files:
            emit({"type": "result", "ok": False, "summary": "No Compose files found.", "data": None,
                  "error": {"code": "NO_COMPOSE_FILES",
                            "message": f"No {', '.join(PATTERNS)} files in {root}.", "retryable": False}})
            return 1
        rows, service_count, port_count = [], 0, 0
        for path in files:
            services = parse_compose(path.read_text(encoding="utf-8"))
            emit({"type": "log", "level": "info", "text": f"{path.name}: {len(services)} service(s)"})
            for service, ports in services.items():
                if only_published and not any(p["published"] for p in ports):
                    continue
                service_count += 1
                entries = ports or [{"published": None, "target": None, "protocol": None, "host_ip": None}]
                for index, port in enumerate(entries):
                    port_count += 1 if port["target"] else 0
                    rows.append({"id": f"{path.name}:{service}:{index}", "values": {
                        "file": path.name, "service": service, **port}})
        emit({"type": "view", "view_id": "ports", "op": "replace", "data": {
            "kind": "table",
            "columns": [
                {"id": "file", "label": "File", "type": "text"},
                {"id": "service", "label": "Service", "type": "text"},
                {"id": "published", "label": "Published", "type": "text"},
                {"id": "target", "label": "Container", "type": "text"},
                {"id": "protocol", "label": "Protocol", "type": "text"},
                {"id": "host_ip", "label": "Host IP", "type": "text"},
            ],
            "rows": rows,
        }})
        emit({"type": "result", "ok": True,
              "summary": f"{service_count} service entries, {port_count} port mapping(s) in {len(files)} file(s).",
              "data": {"files": [p.name for p in files], "services": service_count, "ports": port_count}})
        return 0
    except (KeyError, ValueError, TypeError, OSError) as exc:
        print(f"scan failed: {exc}", file=sys.stderr)
        emit({"type": "result", "ok": False, "summary": "Could not read the Compose files.", "data": None,
              "error": {"code": "SCAN_FAILED", "message": str(exc), "retryable": False}})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
