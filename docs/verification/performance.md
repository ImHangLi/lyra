# Performance measurements (LYR-16)

Measured on 2026-09-26 on the v0.1.1 candidate (code at `9873668`, plus the version bump) with the release build (`cargo build --release --locked`, thin LTO). The machine was an Apple M5 Pro with 48 GiB, running macOS 27.2. The spec's baseline is an Apple M2 with 16 GiB, so these results are for faster hardware and are not M2 results. Both scripts use a scratch copy of `tests/fixtures/workspace` under `/tmp` and stop everything they start.

Reproduce:

```sh
cargo build --release --locked
python3 scripts/perf/measure.py --lyra target/release/lyra --samples 50 --flood-seconds 60 --flood-rounds 3
python3 -m venv /tmp/lyra-venv && /tmp/lyra-venv/bin/pip install pyte
/tmp/lyra-venv/bin/python scripts/perf/tui.py --lyra target/release/lyra --starts 50 --keys 1000
```

## Results (milliseconds)

| Metric (§19) | Target p95 | n | p50 | p95 | p99 | max | Result |
|---|---:|---:|---:|---:|---:|---:|---|
| Warm TUI to operable first frame | 120 | 50 | 8.8 | 10.1 | 10.6 | 10.8 | met |
| Cold TUI to operable first frame (new host) | 320 | 50 | 51.6 | 53.0 | 61.0 | 68.6 | met |
| Navigation key → screen change, idle | 24 | 1000 | 2.3 | 3.1 | 3.3 | 8.0 | met |
| Navigation during a log flood | 40 | 1000 | 1.8 | 2.8 | 3.4 | 4.5 | met |
| Warm `lyra status` (CLI start + IPC + serialization) | 80 | 50 | 6.5 | 7.5 | 7.9 | 8.2 | met |
| Warm `lyra catalog` | 80 | 50 | 6.6 | 6.9 | 7.2 | 7.3 | met |
| Warm `lyra catalog --if-revision` | 80 | 50 | 6.4 | 6.9 | 7.2 | 7.3 | met |
| Warm-cache bounded discovery (`setup --json`) | 400 | 20 | 4.5 | 5.9 | 11.7 | 13.2 | met (small test workspace; see limits) |
| Cold `lyra status` (starts a host) | not a target | 20 | 47.3 | 49.3 | 62.4 | 65.6 | reported |

**Log flood**: 3 rounds of 60 s at about 5,000 lines/s, 200 bytes per line (`dev.flood`). Each round is one `lyra status` every 200 ms while the flood runs.

| Round | `status` n | p50 | p95 | p99 | max | Lines logged | Dropped | `stop --wait` |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 267 | 19.2 | 20.2 | 20.5 | 21.5 | 300,801 | 0 | 123 ms |
| 2 | 267 | 19.1 | 20.2 | 20.8 | 21.5 | 300,501 | 0 | 124 ms |
| 3 | 267 | 19.2 | 20.1 | 20.4 | 20.6 | 300,901 | 0 | 123 ms |

**Idle resources** (TUI open, host running, no plugin running, 10 s window):

| Metric | Target | Measured |
|---|---|---|
| CPU | < 1% of one core | 0.0% (below the 10 ms `ps` resolution) |
| RSS | ≤ 80 MiB | 32.2 MiB (TUI + host) |

## How these were measured

- **TUI latency:** measured in a real PTY at 120×40, from writing the key to the moment the parsed screen changes. The number includes PTY and parser (pyte) overhead. It measures committed frames, not pixels on a physical display.
- **Operable first frame:** the moment the tool list is drawn and accepts keys. For a cold start, no host is running and the TUI starts one.
- **Earlier fix:** under an extreme synthetic flood (4 runs at 1M lines/s), runner tasks starved the host's worker threads. This was found and fixed in #31: runners now yield per output chunk. The flood here is the spec's 5,000 lines/s.

## Limits

- **Terminal apps:** Terminal.app and iTerm2 were not driven by hand, so the difference between them is not reported. All TUI numbers come from a PTY.
- **Hardware:** only this M5 Pro was measured. There are no M2 or x86_64 numbers.
- **Discovery size:** it was measured on the test workspace (small). A 5000-entry/8 MiB tree was measured once in #24 at 37 ms on a debug build; it has no release percentiles. Cold-disk discovery was not measured.
- **Cold CLI:** the cold `status` numbers include starting a new host process. An earlier run at `c654e14` had a p99 near 400 ms; this run had 62 ms. No target covers this.
- **Output with no newlines:** a separate check streamed 200 MiB with no newline through `lyra exec`. Host RSS peaked at 44 MiB (v0.1.0: 1,641 MiB), and it finished in 5 s (v0.1.0: 75 s). See #43.
