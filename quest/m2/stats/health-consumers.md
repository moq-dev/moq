# [M] Client health in use

## Goal

The client health grade from
[moq-dev/moq#6654](https://github.com/moq-dev/moq/pull/6654) is read by
something and holds for a connection carrying several broadcasts: stats carry
a per-connection media counter, at least one consumer shows the grade, and the
grade's limits are re-measured against browser reporters.

## Plan

- Add a media counter to the per-connection `Transport` stats
  (`rs/hang/src/stats.rs` and the JS mirror). `health.rs` says the grade holds
  only for a connection's sole broadcast until then.
- Consumers: preflight is planned in [Preflight](/quest/m2/stats/preflight.md);
  add the demo dashboard and a `moq` CLI sink.
- Re-measure the thresholds in `rs/hang/src/health.rs` against the numbers
  the browser reporters publish (`<moq-publish stats>` and
  `<moq-watch echo>`).
