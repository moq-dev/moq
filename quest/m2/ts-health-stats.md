# [S] TS health counters ride the stats plumbing

## Goal

`moq import ts --stats` and `moq export ts --echo` publish the `ts::stats`
counters so a dashboard reads the TR 101 290 counters and per-PID liveness
beside the media and delivery counters, with no new transport. No health
roll-up in-tree: green, amber or red per priority is the consumer's reading of
counter rates and PID quiet times.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

The quest audit confirmed this stays in m2 and owns TS counter serialization
and integration. Rust reporters in m1 supplies only the generic per-rendition
plumbing; it does not implement this quest's `mpegts` payloads.

- **Surface through the stats plumbing**, as the issue settled. The
  [media stats schema](/quest/m2/stats/schema.md) settles the shape: an
  import flattens `ts::stats::Snapshot` into the publisher's stats snapshot under the
  catalog's `mpegts` key, the counters stay owned by moq-mux beside
  `ts::Ext`, and `hang` stays TS-free.
- **The export reports through `.echo`** (decided 2026-10-12, from
  [#5360](https://github.com/moq-dev/moq/issues/5360)). An export is a viewer,
  so `moq export ts --echo <name>` folds `ts::stats::Snapshot` into its
  solicited `.echo` snapshot under `mpegts`, through the `E` flatten the
  [media stats schema](/quest/m2/stats/schema.md) gives `echo::Snapshot`.
  The egress is where an operator otherwise cannot see per-PID outages: a
  57 s video hole passed every TR 101 290 P1 check in the reporter's lab,
  and only per-PID access-unit counts caught it. There is no export
  `--stats` flag.
- **Counters sum, gauges do not.** Every check is a cumulative counter, which
  keeps `.z` deltas small and lets an aggregate sum; the PID's `quiet` is a gauge and
  merges newest-wins.
- `ts::stats::Snapshot` gains serde, every field defaulted and unknown fields
  ignored. Its rows' `track` is already an owned `String`, so they deserialize.
- Docs: `doc/concept/stats.md` documents each counter's TR 101 290 check, its
  monitoring point (ingest grades the feed, egress grades our muxer, and
  neither is the groomed wire), the value domain of the PCR checks, and that
  the per-PID rows cover `PID_error`; `doc/bin/cli.md` the flags.
- Test: publish a TS clip with `--stats` and export it with `--echo` to a
  soliciting catalog, read both back, and assert every ETSI
  counter reads zero on a clean clip while every elementary stream's
  access-unit count advances.

## Required

- [Media stats schema](/quest/m2/stats/schema.md) - the snapshot the counters flatten into
- [Rust reporters](/quest/m2/stats/rust.md) - `moq import --stats` and the stats interval
