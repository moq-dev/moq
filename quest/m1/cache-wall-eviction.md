# [S] Plan: wall-clock age-out in the track cache

## Goal

A benchmark decides whether a track's cache ages groups out without waiting
for a write, on `max(wall, pts)` like other time decisions in moq-net, or
stays the one write-driven exception. The result is an implementation quest
or a recorded reason to keep the exception.

## Plan

Settled scope: a track's `max_age` retention aging groups out on a timer
instead of only on a write. The pool's idle expiry is out of scope.

- Today a track's `max_age` is media time and is applied only when the track
  writes: a group ages out when a later one starts (`is_stale` and the expiry
  scans in `rs/moq-net/src/model/track.rs`). A track that stops writing keeps
  groups past `max_age` until the pool's idle expiry (`Pool::gc`, driven by
  the origin driver without a write) or byte pressure reclaims them.
  `max_age_does_not_drive_wall_eviction` pins that behaviour.
- Prototype the alternative behind a bench-only switch: each track keeps a
  deadline for its oldest group on `max(wall elapsed, pts)`, armed on the
  timers the origin driver already runs, and evicts on expiry.
- Bench in `rs/moq-net/benches/track.rs`, swept over tracks (1 to 10k) and
  cached groups per track (1 to 1k): write-path cost, timer cost per driver
  pass, and retained memory for a population of idle tracks. A cost that
  grows with the table should show as a slope.
- Weigh the semantics too: `max_age` is media time on purpose, so a congestion
  stall cannot age content out (`track::Info::max_age`). A wall term changes
  that for a stalled but live publisher.
- Record the numbers and the decision in the PR, then rewrite this quest into
  the implementation or delete it.

Public API: none from the plan. Wire: none.

## Related

- [Cache expiry growth](/quest/m1/cache-expiry-growth.md) - relay memory past the expiry window, in the same cache
- [Cache shard](/quest/m1/perf/cache-shard.md) - the pool's shared counters under many workers
