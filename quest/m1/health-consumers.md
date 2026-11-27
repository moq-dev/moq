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
- Consumers: `moq-dev/studio` m0 (`quest/m0/pin-moq-tip.md`, pinned at `81e43217c`
  until it moves to a `main` where `hang::health` is public), `moq-dev/hang.live` m2
  (`quest/m2/network-xray.md`, reading `Hang.Health`), preflight in
  [Preflight](/quest/m3/stats/preflight.md), plus the demo dashboard and a `moq`
  CLI sink. Promoted to m1 on 2026-11-26 so downstream consumers are unblocked.
- Re-measure the thresholds in `rs/hang/src/health.rs` against the numbers
  the browser reporters publish (`<moq-publish stats>` and
  `<moq-watch echo>`).
