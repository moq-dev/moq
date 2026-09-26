# [M] JS timeline scans

## Goal

A `@moq/net` publisher's per-group cost stays flat as the retained window
grows. `Subscriber.#drift()`, `#reach()`, and the prune pass stop scanning
every cached group.

## Plan

- `js/net/src/track.ts` keeps the timeline in a `Map` of every cached group,
  consumed ones included. `#drift` and `#reach` scan it per popped group and on
  every guard re-evaluation, and `#prune`/`#schedulePrune` scan it per publish.
- Mirror Rust's `rs/moq-net/src/model/track.rs`: keep sequences sorted (append
  fast path, binary insert otherwise). Drift walks backward to the first
  stamped, non-errored group in range; reach binary-searches the successor.
  Fold the prune scans in if the benchmark shows them. Memoizing per revision
  was rejected: a new group still costs O(n) per subscriber. An incremental
  edge was rejected: invalidation on aborts and cursor ends gets fiddly.
- Add `js/net/bench/track.ts`: microseconds per published group through
  Producer, Subscriber, `tryRecvGroup`, and the guard, with no transport. Sweep
  retained groups (about 25 to 1500) against subscribers (1 to 16), plus a case
  with guards in flight. Wire it into `nightly.yml` beside `broadcasts.ts`. The
  fix passes when the retained-groups axis is flat.

## Closes

- [#4246](https://github.com/moq-dev/moq/issues/4246) - per-group publishing cost grows with the retained window

## Related

- [Browser benchmarks](/quest/m1/browser-benchmarks.md) - the browser media path; this bench covers the track model
- [JS group guard](/quest/m1/js-group-guard.md) - the thunk there cuts guard calls, not the scan cost
