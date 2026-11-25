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

It builds `moq-relay` and `moq-cli` with the `jemalloc` feature under the
`profiling` cargo profile, release code with line tables so heap dumps name
their stacks. `STEADY_PROFILE` picks another profile, and `RELAY_BIN` and
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
allocated bytes grow faster than 32 bytes a group; the import must keep reading
and the export keep writing, so a stalled chain is no pass. Resident bytes and
VmRSS are recorded but not judged (see Sizing).

A failing run's directory holds `samples.jsonl`, `report.json`, each process's
log, and `heap/<process>.<pid>.<seq>.i<n>.heap`. Diff the first dump after
warm-up against the last to see which stacks grew:

```bash
jeprof --text --base heap/relay.<pid>.<a>.heap bin/moq-relay heap/relay.<pid>.<b>.heap
```

## Sizing

Measured on 2026-11-25 with the `profiling` build, over moq-lite-07 at 60
groups a second. A seeded leak (the record fold from before
[moq-dev/moq#9579](https://github.com/moq-dev/moq/pull/9579)) was compared
with the fixed build. Each figure is the Theil-Sen slope of allocated bytes,
divided by the group rate:

| Window (s) | Fixed, worst process | Seeded leak, mildest process |
| ---------- | -------------------- | ---------------------------- |
| 180-600    | +22.7 B/group        | +75.1 B/group                |
| 300-900    | +9.3 B/group         | +73.0 B/group                |
| 600-1200   | +10.8 B/group        | +75.7 B/group                |
| 2400-3300  | +0.5 B/group         | (not run)                    |

The import's heap still climbs for its first ten minutes, so the shortest run
that keeps at least a twofold margin on both sides is 900 s judged after
300 s, failing at 32 B/group. Over an hour the fixed build levels off in
every process. Resident bytes and VmRSS can't separate the two at this length:
the leak's VmRSS slope ran +9 to +59 B/group over 300-900 s, while the fixed
relay's swung to +140 B/group over 180-600 s.

The run uses lite-07 because lite-06, which clients negotiate by default,
still grows in the relay and the export
([quest](/quest/m1/lite06-sparse-skips.md)).
