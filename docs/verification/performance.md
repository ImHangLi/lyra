# Performance measurements (LYR-16)

Measured on 2026-09-26 on the v0.2.0 candidate (code at `f44e09e`, plus the version bump) with the release build (`cargo build --release --locked`, thin LTO). The machine was busy with other work (load average about 5), which raises some CLI numbers compared with the v0.1.1 run. The machine was an Apple M5 Pro with 48 GiB, running macOS 27.2. The spec's baseline is an Apple M2 with 16 GiB, so these results are for faster hardware and are not M2 results. Both scripts use a scratch copy of `tests/fixtures/workspace` under `/tmp` and stop everything they start.

Reproduce:

```sh
cargo build --release --locked
python3 scripts/perf/measure.py --mira target/release/mira --samples 50 --flood-seconds 60 --flood-rounds 3
python3 -m venv /tmp/mira-venv && /tmp/mira-venv/bin/pip install pyte
/tmp/mira-venv/bin/python scripts/perf/tui.py --mira target/release/mira --starts 50 --keys 1000
```

## Results (milliseconds)

| Metric (§19) | Target p95 | n | p50 | p95 | p99 | max | Result |
|---|---:|---:|---:|---:|---:|---:|---|
| Warm TUI to operable first frame | 120 | 50 | 13.3 | 14.2 | 15.1 | 15.7 | met |
| Cold TUI to operable first frame (new host) | 320 | 50 | 51.0 | 54.2 | 59.2 | 63.8 | met |
| Navigation key → screen change, idle | 24 | 1000 | 2.5 | 3.0 | 3.2 | 8.8 | met |
| Navigation during a log flood | 40 | 1000 | 2.0 | 2.8 | 3.3 | 10.5 | met |
| Warm `mira status` (CLI start + IPC + serialization) | 80 | 50 | 13.1 | 14.9 | 15.3 | 15.5 | met |
| Warm `mira catalog` | 80 | 50 | 10.5 | 11.3 | 11.3 | 11.3 | met |
| Warm `mira catalog --if-revision` | 80 | 50 | 9.9 | 10.7 | 10.8 | 10.8 | met |
| Warm-cache bounded discovery (`setup --json`) | 400 | 20 | 6.3 | 7.1 | 13.9 | 15.7 | met (small test workspace; see limits) |
| Cold `mira status` (starts a host) | not a target | 20 | 45.2 | 49.6 | 67.7 | 72.3 | reported |

**Log flood**: 3 rounds of 60 s at about 5,000 lines/s, 200 bytes per line (`dev.flood`). Each round is one `mira status` every 200 ms while the flood runs.

| Round | `status` n | p50 | p95 | p99 | max | Lines logged | Dropped | `stop --wait` |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 269 | 20.1 | 21.0 | 21.7 | 22.7 | 300,301 | 0 | 124 ms |
| 2 | 270 | 19.8 | 20.9 | 21.2 | 21.4 | 300,901 | 0 | 123 ms |
| 3 | 269 | 20.3 | 21.4 | 22.0 | 23.2 | 300,301 | 0 | 125 ms |

**Idle resources** (TUI open, host running, no plugin running, 10 s window):

| Metric | Target | Measured |
|---|---|---|
| CPU | < 1% of one core | 0.1% |
| RSS | ≤ 80 MiB | 31.7 MiB (TUI + host) |

## How these were measured

- **TUI latency:** measured in a real PTY at 120×40, from writing the key to the moment the parsed screen changes. The number includes PTY and parser (pyte) overhead. It measures committed frames, not pixels on a physical display.
- **Operable first frame:** the moment the tool list is drawn and accepts keys. For a cold start, no host is running and the TUI starts one.
- **Earlier fix:** under an extreme synthetic flood (4 runs at 1M lines/s), runner tasks starved the host's worker threads. This was found and fixed in #31: runners now yield per output chunk. The flood here is the spec's 5,000 lines/s.

## Limits

- **Terminal apps:** Terminal.app and iTerm2 were not driven by hand, so the difference between them is not reported. All TUI numbers come from a PTY.
- **Hardware:** only this M5 Pro was measured. There are no M2 or x86_64 numbers.
- **Discovery size:** it was measured on the test workspace (small). A 5000-entry/8 MiB tree was measured once in #24 at 37 ms on a debug build; it has no release percentiles. Cold-disk discovery was not measured.
- **Cold CLI:** the cold `status` numbers include starting a new host process. An earlier run at `c654e14` had a p99 near 400 ms; this run had 62 ms. No target covers this.
- **Load:** under the same load, a back-to-back run of 60 warm `mira status` calls measured p50 9.4 ms and p95 10.4 ms for the previous build, and p50 10.1 ms and p95 12.0 ms for v0.2.0.
- **Output with no newlines:** a separate check streamed 200 MiB with no newline through `mira exec`. Host RSS peaked at 44 MiB (v0.1.0: 1,641 MiB), and it finished in 5 s (v0.1.0: 75 s). See #43.
