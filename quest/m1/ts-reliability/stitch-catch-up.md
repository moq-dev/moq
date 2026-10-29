# [M] Decide whether a stitched export demotes its old leg

## Goal

Decide, by measurement, whether a stitched export should demote its old leg's
track subscriptions while the replacement acquires, and land the demotion if it
shortens the switch's output hole on the bufferbloat fixture.

## Plan

The latency half landed in
[moq-dev/moq#7049](https://github.com/moq-dev/moq/pull/7049): a stitched
switch drops the old program's tail and starts the replacement at `--delay`.
The priority half landed in
[moq-dev/moq#7145](https://github.com/moq-dev/moq/pull/7145): a mid-stream
`track::Subscriber::update` now reaches the publisher on lite and
moq-transport, directly and through a relay, and on an old instance after an
epoch switch (`priority_update_*` in
`rs/moq-net-tests/tests/integration/restart.rs`).

What remains is the experiment
[moq-dev/moq#6919](https://github.com/moq-dev/moq/pull/6919) motivated: it
measured the replacement's keyframe arriving 0.7 to 2.2 s after the follow ran
out on a 160 kbit/s link, where both legs' audio outranks all video. Demoting
the old leg (for example to priority -1 from the moment `Follower::poll_switch`
starts subscribing the replacement) lets the replacement's keyframe through
sooner, but under strict priority the old leg then starves on a link slower
than either leg alone, so its output may stall before the switch. Whether the
hole shrinks is empirical.

- Plumb a priority change from `Follower` through the export's program tracks
  (`rs/moq-mux/src/container/ts/export.rs`, `crate::Source`) to their
  `track::Subscriber`s or `Control`s.
- Compare `redundant_importers_switch_through_bufferbloat_on_lite_07` in
  `rs/moq-cli/tests/integration/export.rs` with and without the demotion:
  the switch's output gap and the replacement's keyframe arrival.
- Land it only if the gap shrinks; otherwise delete this quest and record the
  numbers in the PR.

Public API: none expected. Wire: none.
