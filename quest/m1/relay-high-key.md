# [M] A publisher's far-ahead group key costs a relay nothing extra

## Goal

A publisher that sends one group with a huge key and then keeps publishing
below it makes a relay hold memory bounded by what it caches, and each
subscriber wake costs work bounded by what that subscriber can still be
handed, not by every group the track ever received. A subscriber asking a
broadcast for endless track names grows nothing once each name is forgotten.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source; reproduce
first. Both are on `release` too.

- `rs/moq-net/src/model/track.rs`: `insert_group` makes the highest key the
  live edge, `protects` keeps it from eviction, and the arrival trim only pops
  from the front, so behind a protected `1<<40` group nothing is ever trimmed
  (16 B per group forever). `poll_recv_group` skips entries below a reader's
  `min_key` without advancing its index, so every insert wakes every live
  subscriber into a walk over all N stale entries under the track mutex:
  quadratic CPU on the relay's shared runtime. Recommended: compact
  `arrival` the way `evict` is rebuilt, and let a reader's cursor skip
  entries it will never be handed.
- `rs/moq-net/src/model/front.rs`: the front's track-info map keeps entries
  past `Forgotten`; drop them there.

Recommended 2026-11-17 (agent, under /quest-afk, for the maintainer's
review): ✅ backport the track fix to `release` as a separate PR after it
lands on `main`, per RELEASE.md, since one publisher can burn a shared
relay's CPU (rejected: shipping it only with the cut).

Test: a publisher writes key `1<<40`, then keys 0..N; assert the arrival
queue stays bounded and a live subscriber's per-wake scan does not grow with
N. Add a bench sweep over N and subscribers, per AGENTS.md's fan-out rule.

Public API: none. Wire: none.
