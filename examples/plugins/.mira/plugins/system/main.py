"""MPP/1 plugin: a small system monitor for macOS (CPU, memory, disk, processes).

Every source is a cheap local call: `ps`, `sysctl`, `vm_stat`, and `os.statvfs`.
CPU use comes from the change in each process's CPU time between two `ps`
samples, so one sample costs a few tens of milliseconds of CPU.
"""

import json
import os
import signal
import subprocess
import sys
import time

SECTIONS = ["cpu", "memory", "disk", "processes"]
TABLE_ROWS = 15
GIB = 1024 ** 3
MIB = 1024 ** 2
LABEL_WIDTH = 7
NAME_WIDTH = 26
FULL, EMPTY = "█", "░"


def emit(frame):
    line = json.dumps({"api": 1, **frame}, ensure_ascii=False, allow_nan=False)
    print(line, flush=True)


def run(argv):
    return subprocess.run(argv, capture_output=True, text=True, timeout=10, check=True).stdout


# --- Sources -----------------------------------------------------------------


def cpu_seconds(text):
    """Parses ps `time` values such as `1:02.50`, `12:01:02`, or `2-03:04:05`."""
    days, _, rest = text.rpartition("-")
    total = 0.0
    for part in rest.split(":"):
        total = total * 60 + float(part)
    return total + (int(days) * 86400 if days else 0)


def processes():
    """Returns {pid: (name, cpu_seconds, rss_bytes)} for every process."""
    out = run(["/bin/ps", "-A", "-o", "pid=,time=,rss=,comm="])
    procs = {}
    for line in out.splitlines():
        fields = line.split(None, 3)
        if len(fields) < 4:
            continue
        pid, used, rss, comm = fields
        try:
            procs[int(pid)] = (os.path.basename(comm.strip()), cpu_seconds(used), int(rss) * 1024)
        except ValueError:
            continue
    return procs


def sysctl(*names):
    return run(["/usr/sbin/sysctl", "-n", *names]).splitlines()


def memory(total):
    """Used memory the way Activity Monitor counts it: app + wired + compressed."""
    out = run(["/usr/bin/vm_stat"])
    lines = out.splitlines()
    page = int(lines[0].split("page size of")[1].split()[0])
    pages = {}
    for line in lines[1:]:
        key, _, value = line.partition(":")
        value = value.strip().rstrip(".")
        if value.isdigit():
            pages[key.strip().strip('"')] = int(value)
    app = pages.get("Anonymous pages", 0) - pages.get("Pages purgeable", 0)
    used = (app + pages.get("Pages wired down", 0) + pages.get("Pages occupied by compressor", 0)) * page
    cached = (pages.get("File-backed pages", 0) + pages.get("Pages purgeable", 0)) * page
    return {"used": used, "total": total, "cached": cached}


def disk(path="/"):
    """Capacity of the volume that holds `path`; used = total - available, as Finder shows it."""
    st = os.statvfs(path)
    total = st.f_blocks * st.f_frsize
    free = st.f_bavail * st.f_frsize
    return {"path": path, "used": total - free, "free": free, "total": total}


def sample(before, before_at):
    """One full sample. `before` is an earlier process sample to measure CPU against."""
    after = processes()
    now = time.monotonic()
    elapsed = max(now - before_at, 0.001)
    memsize, ncpu, loadavg = sysctl("hw.memsize", "hw.ncpu", "vm.loadavg")
    cores = int(ncpu)
    rows = []
    busy = 0.0
    for pid, (name, used, rss) in after.items():
        prev = before.get(pid)
        delta = used - prev[1] if prev and prev[0] == name else 0.0
        delta = max(delta, 0.0)
        busy += delta
        rows.append({"pid": pid, "name": name, "cpu": 100.0 * delta / elapsed, "rss": rss})
    rows.sort(key=lambda r: (-r["cpu"], -r["rss"]))
    stats = {
        "cpu": {
            "percent": min(100.0, 100.0 * busy / (elapsed * cores)),
            "cores": cores,
            "load": [float(x) for x in loadavg.strip("{} ").split()[:3]],
        },
        "memory": memory(int(memsize)),
        "disk": disk(),
        "processes": rows,
        "at": time.strftime("%H:%M:%S"),
    }
    return stats, after, now


# --- Rendering ---------------------------------------------------------------


def bar(percent, width):
    filled = round(width * max(0.0, min(percent, 100.0)) / 100)
    return FULL * filled + EMPTY * (width - filled)


def meter(label, percent, width, detail):
    return f"{label:<{LABEL_WIDTH}}{bar(percent, width)} {percent:4.0f}%   {detail}"


def clip(text, width):
    return text if len(text) <= width else text[: width - 1] + "…"


def gib(n):
    return f"{n / GIB:.1f}"


def overview(stats, settings, interval=None):
    width, top = settings["bar_width"], settings["top"]
    every = f", every {interval} s" if interval else ""
    lines = [f"Updated {stats['at']}{every}", ""]
    for section in settings["sections"]:
        if section == "cpu":
            c = stats["cpu"]
            load = " ".join(f"{x:.2f}" for x in c["load"])
            lines.append(meter("CPU", c["percent"], width, f"{c['cores']} cores, load {load}"))
        elif section == "memory":
            m = stats["memory"]
            pct = 100.0 * m["used"] / m["total"]
            detail = f"{gib(m['used'])} / {gib(m['total'])} GiB, cached {gib(m['cached'])}"
            lines.append(meter("Memory", pct, width, detail))
        elif section == "disk":
            d = stats["disk"]
            pct = 100.0 * d["used"] / d["total"]
            detail = f"{d['used'] / GIB:.0f} / {d['total'] / GIB:.0f} GiB, free {d['free'] / GIB:.0f}"
            lines.append(meter("Disk", pct, width, detail))
        elif section == "processes":
            if lines[-1] != "":
                lines.append("")
            # A fixed name column keeps the other columns still between samples.
            lines.append(f"{'Top processes':<{NAME_WIDTH}}  {'PID':>6}  {'CPU':>6}  {'Memory':>10}")
            for p in stats["processes"][:top]:
                lines.append(
                    f"{clip(p['name'], NAME_WIDTH):<{NAME_WIDTH}}  {p['pid']:>6}  {p['cpu']:5.1f}%"
                    f"  {p['rss'] / MIB:>6.0f} MiB"
                )
    return {"kind": "text", "text": "\n".join(lines).rstrip() + "\n", "format": "plain"}


def table(stats):
    rows = [
        {
            "id": str(p["pid"]),
            "values": {
                "pid": p["pid"],
                "name": p["name"],
                "cpu": round(p["cpu"], 1),
                "memory": round(p["rss"] / MIB, 1),
            },
        }
        for p in stats["processes"][:TABLE_ROWS]
    ]
    columns = [
        {"id": "pid", "label": "PID", "type": "number"},
        {"id": "name", "label": "Name", "type": "text"},
        {"id": "cpu", "label": "CPU %", "type": "number"},
        {"id": "memory", "label": "Memory MiB", "type": "number"},
    ]
    return {"kind": "table", "columns": columns, "rows": rows}


def publish(stats, settings, interval=None):
    emit({"type": "view", "view_id": "overview", "op": "replace", "data": overview(stats, settings, interval)})
    emit({"type": "view", "view_id": "processes", "op": "replace", "data": table(stats)})


# --- Actions -----------------------------------------------------------------


def settings_from(req):
    given = req.get("input") or {}
    sections = [s for s in given.get("sections", SECTIONS) if s in SECTIONS]
    return {
        "interval_s": int(given.get("interval_s", 2)),
        "bar_width": int(given.get("bar_width", 24)),
        "top": int(given.get("top", 5)),
        "sections": sections or SECTIONS,
    }


def watch(settings):
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    interval = settings["interval_s"]
    before, before_at = processes(), time.monotonic()
    time.sleep(min(interval, 1))
    healthy = False
    while True:
        try:
            stats, before, before_at = sample(before, before_at)
            publish(stats, settings, interval)
            if not healthy:
                emit({"type": "status", "state": "healthy", "message": f"sampling every {interval} s"})
                healthy = True
        except (OSError, ValueError, IndexError, subprocess.SubprocessError) as exc:
            print(f"sample failed: {exc}", file=sys.stderr)
            emit({"type": "status", "state": "warn", "message": f"sample failed: {exc}"})
            healthy = False
        time.sleep(interval)


def snapshot(settings):
    before, before_at = processes(), time.monotonic()
    time.sleep(1)
    stats, _, _ = sample(before, before_at)
    publish(stats, settings)
    c, m, d = stats["cpu"], stats["memory"], stats["disk"]
    mem_pct = 100.0 * m["used"] / m["total"]
    disk_pct = 100.0 * d["used"] / d["total"]
    summary = (
        f"CPU {c['percent']:.0f}% of {c['cores']} cores, memory {mem_pct:.0f}%"
        f" ({gib(m['used'])} / {gib(m['total'])} GiB), disk {disk_pct:.0f}% ({d['free'] / GIB:.0f} GiB free)."
    )
    top = [
        {"pid": p["pid"], "name": p["name"], "cpu_percent": round(p["cpu"], 1), "memory_mib": round(p["rss"] / MIB, 1)}
        for p in stats["processes"][: settings["top"]]
    ]
    data = {
        "cpu": {"percent": round(c["percent"], 1), "cores": c["cores"], "load": c["load"]},
        "memory": {"used_bytes": str(m["used"]), "total_bytes": str(m["total"]), "percent": round(mem_pct, 1)},
        "disk": {
            "path": d["path"], "used_bytes": str(d["used"]), "free_bytes": str(d["free"]),
            "total_bytes": str(d["total"]), "percent": round(disk_pct, 1),
        },
        "top": top,
    }
    emit({"type": "result", "ok": True, "summary": summary, "data": data})


def main():
    req = json.loads(sys.stdin.readline())
    settings = settings_from(req)
    if req["action"] == "watch":
        watch(settings)
        return 0
    try:
        snapshot(settings)
        return 0
    except (OSError, ValueError, IndexError, subprocess.SubprocessError) as exc:
        print(f"snapshot failed: {exc}", file=sys.stderr)
        error = {"code": "SAMPLE_FAILED", "message": str(exc), "retryable": True}
        emit({"type": "result", "ok": False, "summary": "Could not read the system counters.", "data": None, "error": error})
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
