# [S] Parked reads wake

## Goal

A read parked on a group that is evicted or aborted always wakes and
re-judges, for resume successors and plain tracks alike, pinned by mock-time
regression tests.

## Plan

Follow-ups from [#4484](https://github.com/moq-dev/moq/pull/4484), which made
`resume::Successor` recompute its answer on every judgment:

- Its regression tests cover an aborted unstamped successor only. Eviction
  wakes through a different path, so add a resume-level test where the
  successor is evicted before its first frame. If it already passes, this
  item lands as a test.
- A stamped group that is aborted without being evicted wakes no parked
  reader, on a plain track as well as a successor. It only matters under a
  timestamp rewind. Reproduce it first, then wake the waiters at the source.

Settled: one quest, since both are the same wakeup invariant in
`rs/moq-net/src/model/track.rs` and `resume.rs`.

Public API: none. Wire: none.

## Related

- [#4491](https://github.com/moq-dev/moq/pull/4491) - a resumed group holds demand on its replacement copy in the same `resume.rs` wakeups
- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - extends the `resume.rs` takeover tests
