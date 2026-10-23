# [S] Keyed prefix polling

## Goal

A session's `SubscriberDriver` polls only the prefix machines a wake
concerns, so its per-wake cost does not grow with the number of open
prefixes. Or a benchmark shows the cost is flat and this quest is deleted.

## Plan

From [moq-dev/moq#6165](https://github.com/moq-dev/moq/pull/6165).
Decided 2026-10-21 with the maintainer: gated on a measurement.

- First add a prefix-machine bench or extend
  `rs/moq-net/benches/session.rs`, sweeping open prefixes per session
  against wakes. If the
  per-wake cost does not grow with the prefix table, close the quest with
  the numbers.
- Otherwise key polling by prefix and show the slope flatten.

Public API: none. Wire: none.

## Related

- [IETF lazy discovery](/quest/m1/ietf-lazy-discovery.md) - more prefixes per session once IETF is lazy too
