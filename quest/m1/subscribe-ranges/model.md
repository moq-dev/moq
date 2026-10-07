# [L] Model ranges

## Goal

The Rust model's `Subscription` carries a list of group ranges and an order,
aggregated across subscribers. A track serves the union from one cursor
without duplicating a group, in the requested order, and reports every
sequence it won't deliver. A publisher's `track::Dynamic` receives range
requests for misses instead of one request per group.

## Plan

See the [line](/quest/m1/subscribe-ranges/README.md) for the decisions.
Guidance:

- Aggregating several subscribers' ranges and orders onto one upstream
  request needs a rule. For example, the union of ranges, and desc wins when
  orders conflict. Decide it and write it down.
- Replace `requested_group` and `fetch_group` rather than keep both. Update
  every caller (relay, moq-archive, hls, the ladder) in the same PR, plus the
  published surfaces built on them, per the cross-package table: moq-ffi
  (`fetch_group`, `requested_group`), moq-c (`poll_requested_group`), every
  binding wrapper and its `doc/lib` page, and `moq fetch` through
  `moq_relay::fetch_group` (added in the 2026-10-05 audit).
- `max_delay` caps every range.
- Benchmark range count and span as separate axes (AGENTS.md fan-out rule).
