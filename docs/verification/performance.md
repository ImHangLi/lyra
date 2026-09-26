# Performance measurements (LYR-16)

Measured on 2026-09-26 at commit `c654e14` with the release build (`cargo build --release --locked`, thin LTO). The machine was an Apple M5 Pro with 48 GiB, running macOS 27.2. The spec's baseline is an Apple M2 with 16 GiB, so these results are for faster hardware and are not M2 results. Both scripts use a scratch copy of `examples/playground` under `/tmp` and stop everything they start.

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
| Warm TUI to operable first frame | 120 | 50 | 9.9 | 11.4 | 13.5 | 14.2 | met |
| Cold TUI to operable first frame (new host) | 320 | 50 | 50.0 | 51.9 | 58.1 | 63.7 | met |
| Navigation key → screen change, idle | 24 | 1000 | 2.2 | 3.0 | 3.1 | 3.3 | met |
| Navigation during a log flood | 40 | 1000 | 1.7 | 2.8 | 3.2 | 4.3 | met |
| Warm `lyra status` (CLI start + IPC + serialization) | 80 | 50 | 6.9 | 7.5 | 7.7 | 7.8 | met |
| Warm `lyra catalog` | 80 | 50 | 6.9 | 7.4 | 7.5 | 7.6 | met |
| Warm `lyra catalog --if-revision` | 80 | 50 | 6.8 | 7.2 | 7.3 | 7.3 | met |
| Warm-cache bounded discovery (`setup --json`) | 400 | 20 | 4.7 | 5.9 | 13.9 | 15.8 | met (small playground; see limits) |
| Cold `lyra status` (starts a host) | not a §19 target | 20 | 46.5 | 69.6 | 396.4 | 478.1 | reported |

**Log flood**: 3 rounds of 60 s at about 5,000 lines/s, 200 bytes per line (`dev.flood`). Each round is one `lyra status` every 200 ms while the flood runs.

| Round | `status` n | p50 | p95 | p99 | max | Lines logged | Dropped | `stop --wait` |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 272 | 19.0 | 20.4 | 20.8 | 23.4 | 301,101 | 0 | 125 ms |
| 2 | 269 | 19.6 | 20.4 | 20.8 | 21.6 | 301,001 | 0 | 120 ms |
| 3 | 266 | 18.5 | 20.2 | 20.6 | 21.7 | 300,901 | 0 | 121 ms |

**Idle resources** (TUI open, host running, no plugin running, 10 s window):

| Metric | Target | Measured |
|---|---|---|
| CPU | < 1% of one core | 0.0% (below the 10 ms `ps` resolution) |
| RSS | ≤ 80 MiB | 32.3 MiB (TUI + host) |

## How these were measured

- **TUI latency:** measured in a real PTY at 120×40, from writing the key to the moment the parsed screen changes. The number includes PTY and parser (pyte) overhead. It measures committed frames, not pixels on a physical display.
- **Operable first frame:** the moment the tool list is drawn and accepts keys. For a cold start, no host is running and the TUI starts one.
- **Earlier fix:** under an extreme synthetic flood (4 runs at 1M lines/s), runner tasks starved the host's worker threads. This was found and fixed in #31: runners now yield per output chunk. The flood here is the spec's 5,000 lines/s.

## Limits

- **Terminal apps:** Terminal.app and iTerm2 were not driven by hand, so the difference between them is not reported. All TUI numbers come from a PTY.
- **Hardware:** only this M5 Pro was measured. There are no M2 or x86_64 numbers.
- **Discovery size:** it was measured on the playground (small). A 5000-entry/8 MiB tree was measured once in #24 at 37 ms on a debug build; it has no release percentiles. Cold-disk discovery was not measured.
- **Cold CLI tail:** the cold `status` p99 (≈400 ms) includes starting a new host process. The median was 46 ms. No §19 target covers this.
