# Steady broadcast memory

One live broadcast through the TS passthrough chain, run until warm-up is long
past, failing when any process's memory keeps growing:

```text
ffmpeg (live, every frame an IDR) | moq import ts --passthrough -> moq-relay
    -> moq export ts --passthrough > /dev/null
```

The passthrough cuts a group at each random access point, so 60 frames a second
is 60 groups a second: ten minutes of the run is ten hours of a broadcast that
cuts a group a second. Growth that scales with groups shows in minutes.

## Running

```bash
just test steady
just test steady --seconds 1200 --fps 120
```

`cargo build --release -p moq-relay -p moq-cli` builds both with the `jemalloc`
feature. `STEADY_PROFILE` picks the cargo profile, and `RELAY_BIN` and
`MOQ_BIN` point at prebuilt binaries instead, which must have that feature. A
failing run keeps its run directory; a passing run deletes it unless
`HARNESS_KEEP=1`.

## What it judges

Every `--interval` seconds, [`lib/growth.py`](../lib/growth.py) reads each
process's VmRSS and sends it SIGUSR1, which a `jemalloc` build answers by
logging its exact allocated and resident bytes (`moq_tokio::jemalloc`) and
dumping a heap profile. After `--warmup`, it fits each process's slope with the
Theil-Sen estimator, the median slope between every pair of samples, which
in-flight media swinging between samples barely moves. A process fails when its
allocated bytes grow faster than 8 bytes a group, or its resident set faster
than 64 a group; the import must keep reading and the export keep writing, so a
stalled chain is no pass.

A failing run's directory holds `samples.jsonl`, `report.json`, each process's
log, and `heap/<process>.<pid>.<seq>.i<n>.heap`. Diff the first dump after
warm-up against the last to see which stacks grew:

```bash
jeprof --text --base heap/relay.<pid>.<a>.heap bin/moq-relay heap/relay.<pid>.<b>.heap
```

## Sizing

<!-- Filled in from the seeded-leak measurement. -->
