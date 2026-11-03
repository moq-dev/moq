# [M] Ordered reads can skip owed holes, and snapshot latest() does

## Goal

moq-net's `Ordered` gains a read that skips past an owed hole to the newest
buffered group, while keeping the reader's bounds (range, budget, delivered
set). `moq_json::snapshot::Consumer::latest()` moves onto it, so on a
received track with a nonzero or unlimited `max_delay` it returns the newest
buffered snapshot at once instead of waiting for an owed lower group. The
`poll_latest` caveat goes. `@moq/net`'s `Ordered` and `@moq/json`'s
`latest()` do the same.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403).

Milestone: ✅ m1 (moved from m2). Not chosen: m2.

- [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403) decision 7, taken overnight on the maintainer's standing
  authorization (2026-10-31): ✅ land with the documented caveat (the
  `poll_latest` rustdoc, commit
  [1aad05c565](https://github.com/moq-dev/moq/commit/1aad05c565)) and plan
  this follow-up. Not chosen: fix it in [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403), threading new public API
  through `Cursor::poll_next_with`, `Subscriber::poll_group`, and the resume
  layer's multi-copy peek (`rs/moq-net/src/model/resume.rs`).
- Today `latest()` drains `Ordered::poll_next_group`, which since [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403) parks
  above an owed hole until it lands, the track says it won't, or the budget
  gives up. Every in-repo snapshot reader uses the zero default and is
  unaffected; a reader with a budget is not.
- The new read is that threading: skip owed holes but not the reader's range
  or budget, and never hand out a group twice. The caveat sits at
  `rs/moq-json/src/snapshot/consumer.rs:84` at be39188000.
- [moq-dev/moq#7609](https://github.com/moq-dev/moq/pull/7609) (merged) made
  JS ordered reads wait on owed groups too, so `@moq/json`'s `latest()`
  (`js/json/src/snapshot/consumer.ts:59`, draining `Ordered.tryNextGroup`)
  now parks the same way. Mirror the new read in `@moq/net`'s `Ordered`
  (`js/net/src/track.ts:3111`) and move `latest()` onto it.

Public API: adds an `Ordered` read (Rust and JS); `snapshot::Consumer::latest()`
and `@moq/json`'s `latest()` stop waiting at owed holes. Wire: none.
