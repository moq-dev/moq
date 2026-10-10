# [S] moq-bench readiness

## Goal

Every `moq-bench` JSONL line carries the run's rolled targets (connections,
broadcasts, subscriptions) and a `ready` flag, set once every rolled
connection, broadcast, and subscription is established. A harness starts its
measurement window on the first `ready` line instead of parsing logs or
guessing a ramp.

## Plan

Decided 2026-10-10 by maintainer priority:

- **Why.** `Rolled` (`rs/moq-bench/src/connection.rs`) is drawn per
  connection in `main.rs` and only the media keyframe carries it, so the
  JSONL has live gauges (`connections`, `broadcasts`, `subscriptions`) but
  nothing to compare them to.
- **Targets** are the sums over every connection's roll, fan-out roles
  included, fixed at startup.
- **`ready` latches.** It marks the window start; a later loss shows as a
  gauge below its target, not as `ready` turning off.
- **Established** needs one definition per kind. A subscription's gauge rises
  when it starts, not when data arrives; pick whichever matches "measurable"
  (likely its first group) and document it.
- **Reachable.** A mesh connection subscribes to up to `subscribe` peer
  broadcasts, so a roll above what the run publishes never fills. Clamp the
  target to what can exist, or refuse the configuration; never emit a target
  that can never be met.
- Test: a small run's lines read `ready: false` until its gauges reach the
  targets, then `ready: true`; an unreachable roll is clamped or refused.
  Document the fields in `rs/moq-bench/README.md` ("Machine-readable
  output").

Public API: none (CLI output only). Wire: none.

## Related

- [Mergeable bench buckets](/quest/m1/bench-buckets.md) - the same JSONL
  lines; a harness sums buckets over the window `ready` opens
