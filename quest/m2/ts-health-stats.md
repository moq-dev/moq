# [S] TS health counters ride the client stats broadcast

## Goal

`moq import --stats` and `moq export --stats` carry the TS counters as a `ts`
section of the entry they report: import on the publisher entry, export on the
subscriber entry. A dashboard reads the TR 101 290 counters from the same
`.stats` broadcast as the media and delivery counters, with no new track or
transport. No health roll-up in-tree: green, amber or red per priority is the
consumer's reading of counter rates and PID gaps.

## Plan

Decided while planning [#1838](https://github.com/moq-dev/moq/issues/1838):

- **Surface through `moq-stats`**, as the issue settled, on the extension the
  client stats line adds. `ts::Stats` derives `Serialize` and `Deserialize`,
  every field defaulted, and the CLI composes it beside `hang::Stats` as an
  optional `ts` field, so `hang` stays TS-free and a consumer that does not
  know the section ignores it.
- **Counters sum, gauges do not.** Every check is a cumulative counter, which
  keeps `.z` deltas small and lets the aggregate sum clients, as the schema
  quest specifies; the PID gap is a gauge and merges newest-wins.
- **Counters only, no last-event timestamp.** The sample in which a counter
  moved dates the event to the stats interval.
- Targets `dev` with the stats line it builds on.
- Docs: `doc/concept/stats.md` documents the section, each counter's TR 101
  290 check, and its monitoring point (ingest grades the feed, egress grades
  our muxer, and neither is the groomed wire); `doc/bin/cli.md` the flags.
- Test: publish a TS clip with `--stats`, read it back with
  `moq export stats`, and assert the `ts` section is present, zero on a clean
  clip, with every elementary stream's access units advancing.

## Required

- [Rust reporters](/quest/m1/qos/stats/rust.md) - `moq import --stats` and `moq export --stats`
- [TS import health](/quest/m2/ts-import-health.md) - the ingest counters
- [TS export health](/quest/m2/ts-export-health.md) - the egress counters
