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
- **Established** (decided 2026-10-11 by the maintainer). A subscription
  counts once it reads its first group end to end, tracked in a new
  per-subscription gauge; the existing `subscriptions` gauge, which rises at
  `drain` entry before `subscribe().await`, is unchanged. A broadcast counts
  once its connection is connected or it writes its first group, not when
  `produce` starts (today's `broadcasts` gauge rises before the session
  connects). A connection counts on `Status::Connected`, as today.
- **Idle tracks are refused.** An fps of 0 never writes a group, so a
  subscriber of it never becomes established. Refuse a configuration that
  subscribes to media it generates (a mesh run that publishes, or fan-out)
  while fps can roll 0. A subscribe-only run's local fps is unused; its
  external publisher decides whether groups arrive.
- **Unreachable rolls are refused** (decided 2026-10-11 by the maintainer).
  A mesh connection discovers only its run's namespace when the run
  publishes, so it can subscribe to at most the others' broadcasts. Refuse
  at startup when the largest `subscribe` exceeds
  `(connections.min - 1) * broadcasts.min`, each bound taken as
  `Range::sample` normalizes it (an inverted range swaps its ends) and the
  connection count at least 1, as `main.rs` rolls it. The check uses the
  ranges, not the roll, so a configuration is always accepted or always
  refused. Never
  clamp: a smaller roll would measure a lighter load than requested.
- **External publishers skip the check.** A subscribe-only run
  (`broadcasts` 0) discovers broadcasts from other invocations, which it
  cannot count ahead. Its target is the requested sum, and `ready` stays
  false until that is met.
- Test: a small run's lines read `ready: false` until its gauges reach the
  targets, then `ready: true`; an unreachable range, the same range
  inverted, and a publishing run that subscribes with fps 0 are each refused
  at startup; a subscribe-only run with local fps 0 is accepted and reports
  the requested sum as its target. Document the fields in
  `rs/moq-bench/README.md` ("Machine-readable output").

Public API: none (CLI output only). Wire: none.

## Related

- [Mergeable bench buckets](/quest/m1/bench-buckets.md) - the same JSONL
  lines; a harness sums buckets over the window `ready` opens
