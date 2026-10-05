# [M] One max_age meaning for both languages' track caches

## Goal

`max_age` means the same thing in Rust and js/net. A swept benchmark decides
first whether a track's cache ages groups out without waiting for a write, on
`max(wall, pts)` like other time decisions in moq-net, or stays the one
write-driven exception. Both languages then implement that answer, with the
pool's (Rust) or cache window's (JS) wall-clock idle bound kept separate from
`max_age`.

## Plan

Decided in the 2026-10-05 audit: the Rust plan (wall-clock age-out) and the
JS fix (a cache window apart from `maxAge`) merge into this one quest, which
owns `max_age` semantics for both languages. The benchmark decides first;
[#4659](https://github.com/moq-dev/moq/pull/4659), which already moves JS to
media-time `maxAge` with a private 30 s idle window, is reworked to match the
decision or closed after it. Rejected: treating the JS half as settled and
narrowing the benchmark to Rust.

Settled scope: a track's `max_age` retention, and whether it ages groups out
on a timer instead of only on a write. The pool's idle expiry stays the
separate wall-clock bound.

- Today Rust's `max_age` is media time and is applied only when the track
  writes: a group ages out when a later one starts (`is_stale` and the expiry
  scans in `rs/moq-net/src/model/track.rs`). A track that stops writing keeps
  groups past `max_age` until the pool's idle expiry (`Pool::gc`, driven by
  the origin driver without a write) or byte pressure reclaims them.
  `max_age_does_not_drive_wall_eviction` pins that behaviour.
- JS does the opposite on `main`: `#prune` in `js/net/src/track.ts` evicts a
  group once it has been idle on the wall clock (`performance.now`) past
  `maxAge`, on its own timer, so a congestion stall can age content out of a
  JS publisher's cache. `js/net/bench/track.ts` benches the publish cost
  against the retained window.
- Prototype the Rust alternative behind a bench-only switch: each track keeps
  a deadline for its oldest group on `max(wall elapsed, pts)`, armed on the
  timers the origin driver already runs, and evicts on expiry.
- Bench in `rs/moq-net/benches/track.rs`, swept over tracks (1 to 10k) and
  cached groups per track (1 to 1k): write-path cost, timer cost per driver
  pass, and retained memory for a population of idle tracks. A cost that
  grows with the table should show as a slope.
- Weigh the semantics too: `max_age` is media time on purpose, so a congestion
  stall cannot age content out (`track::Info::max_age`). A wall term changes
  that for a stalled but live publisher.
- Whatever this decides, an untimed group is never media-stale and only the
  pool's expiry reclaims it, as decided in
  [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md).
- Then implement the decision in both languages: give js/net a cache window
  of its own for idle eviction, as Rust's pool has (keep it private), and
  apply `max_age` the way the benchmark settled. Test with mocked time that a
  stalled track keeps or drops its groups as decided.
  [Generated @moq/net](/quest/m1/rs2ts/README.md) will replace the JS model
  later; the maintainer chose to fix it by hand first.

Public API: none unless the JS window becomes configurable. Wire: none.

## Related

- [Cache expiry growth](/quest/m1/cache-expiry-growth.md) - relay memory past the expiry window, in the same cache
- [Cache shard](/quest/m2/cache-shard.md) - the pool's shared counters under many workers
- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - untimed groups are never media-stale
- [Generated @moq/net](/quest/m1/rs2ts/README.md) - retires js/net's hand-written model
