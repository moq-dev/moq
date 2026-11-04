# [XS] One page-wide getStats sampling ticker

## Goal

A page's `@moq/net` connections sample their send-rate estimate from one
shared ticker instead of one loop each, so many publishing connections on a
page cost one tick's work per interval.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2, in this epic

From [moq-dev/moq#7524](https://github.com/moq-dev/moq/pull/7524), which
sampled `getStats()` only while a reservation reads it: loops ticking in
phase cost about a third of the main-thread time for 4 to 16 publishing
connections. Observed there, not implemented.

- One module-level ticker that runs only while some connection has a
  reservation reading its estimate, and samples each such connection per
  tick.
- Measure main-thread time before and after with #7524's harness at 4 and
  16 connections.

Public API: none. Wire: none.

## Related

- [Browser send estimate](/quest/m2/probe/browser-send-estimate.md) - where a browser without `getStats()` gets its estimate
