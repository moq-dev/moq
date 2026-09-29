# [S] TS health counters ride the stats plumbing

## Goal

`moq import ts --stats` and `moq export ts --stats` publish the `ts::Stats`
counters so a dashboard reads the TR 101 290 counters and per-PID liveness
beside the media and delivery counters, with no new transport. No health
roll-up in-tree: green, amber or red per priority is the consumer's reading of
counter rates and PID quiet times.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

- **Surface through the stats plumbing**, as the issue settled. The
  [media stats schema](/quest/m1/stats/schema.md) settles the shape: an
  import flattens `ts::Stats` into the publisher's stats snapshot under the
  catalog's `mpegts` key, the counters stay owned by moq-mux beside
  `ts::Ext`, and `hang` stays TS-free.
- Open: an export is a viewer, which has no stats track and reports only
  through a soliciting catalog's `.echo` feedback, so where `moq export ts
  --stats` publishes its egress rows is unsettled.
- **Counters sum, gauges do not.** Every check is a cumulative counter, which
  keeps `.z` deltas small and lets an aggregate sum; the PID's `quiet` is a gauge and
  merges newest-wins.
- `ts::Stats` gains serde, every field defaulted and unknown fields ignored;
  `StreamStats.track` becomes owned so the type deserializes.
- Docs: `doc/concept/stats.md` documents each counter's TR 101 290 check, its
  monitoring point (ingest grades the feed, egress grades our muxer, and
  neither is the groomed wire), the value domain of the PCR checks, and that
  the per-PID rows cover `PID_error`; `doc/bin/cli.md` the flags.
- Test: publish a TS clip with `--stats`, read it back, and assert every ETSI
  counter reads zero on a clean clip while every elementary stream's
  access-unit count advances.

## Required

- [Media stats schema](/quest/m1/stats/schema.md) - the snapshot the counters flatten into
- [Rust reporters](/quest/m1/stats/rust.md) - `moq import --stats` and the stats interval
- [TS import health](/quest/m2/ts-import-health.md) - the ingest counters
- [TS export liveness](/quest/m2/ts-export-liveness.md) - the egress rows
