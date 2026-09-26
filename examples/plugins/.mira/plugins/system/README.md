# System monitor

A small Activity Monitor for macOS, written as a Mira plugin. It shows CPU, memory, disk, and the busiest processes in the same window as your other runs, and an agent reads the same numbers from the CLI.

It uses only the Python 3 standard library and local counters (`ps`, `sysctl`, `vm_stat`, `os.statvfs`). It needs no sudo and no network. One sample costs a few tens of milliseconds of CPU.

## Use it

| Ref | Kind | What it does |
|---|---|---|
| `system.watch` | process | Samples every `interval_s` seconds and replaces the two views. Runs until you stop it. |
| `system.snapshot` | task | Takes one sample, replaces the two views, and returns the numbers as the result. |
| `system.overview` | text view | The dashboard: CPU, memory, and disk bars, then the top processes. |
| `system.processes` | table view | The 15 busiest processes: PID, name, CPU %, memory MiB. |

```sh
mira start system.watch        # keep it running (in the TUI: select "System monitor" and start it)
mira view system.overview      # read the dashboard
mira run system.snapshot       # one sample, no process left running
mira stop system.watch
```

How the numbers are measured:

- **CPU**: the CPU time that all processes used between two `ps` samples, divided by the elapsed time and the number of cores. Process CPU % uses the same measure, so 100% is one full core.
- **Memory**: used = app memory + wired + compressed, as Activity Monitor counts it. "cached" is file-backed and purgeable memory that macOS can free when it needs to.
- **Disk**: the volume that holds `/`. Used = size - available, as Finder shows it, so APFS snapshots and other volumes in the container count as used.

## Change the settings

| Setting | Default | Range | Effect |
|---|---|---|---|
| `interval_s` | 2 | 1 to 60 | Seconds between samples (`watch` only). |
| `bar_width` | 24 | 10 to 60 | Width of each bar, in characters. |
| `top` | 5 | 1 to 20 | Rows in the "Top processes" section. |
| `sections` | `["cpu", "memory", "disk", "processes"]` | any of these | Sections of the dashboard, in this order. |

In the TUI, start the action and fill in the form. Empty fields keep the default. `sections` is a JSON field, for example `["cpu", "processes"]`. The form remembers the values for the next start.

From the CLI, put the settings in a file:

```sh
echo '{"interval_s": 5, "bar_width": 40, "sections": ["cpu", "memory"]}' > /tmp/system.json
mira restart system.watch --input /tmp/system.json   # or `mira start` when it is not running
```

To change a default for everyone on the project, edit the `default` values in `plugin.json` and run `mira reload`.

## Add a metric

The plugin is one short Python file, so you can reshape it or ask your agent to. For example:

> Add a Battery section to the `system` plugin in `.mira/plugins/system`. Read the charge and the power source from `pmset -g batt`, show it as a bar like the others, and add `"battery"` to the `sections` enum and default. Then run `mira validate .mira` and `mira reload`, and show me `mira view system.overview`.

The same pattern works for network throughput (`netstat -ib`, the change in bytes between two samples) or GPU and thermal data. A new metric needs three changes in `main.py`: read it in `sample()`, draw it in `overview()`, and add its name to `SECTIONS`. Add the name to the `sections` enum in `plugin.json` too.
