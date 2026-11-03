# [M] Ordered reads can skip owed holes, and snapshot latest() does

## Goal

moq-net's `Ordered` gains a read that skips past an owed hole to the newest
buffered group, while keeping the reader's bounds (range, budget, delivered
set). `moq_json::snapshot::Consumer::latest()` moves onto it, so on a
received track with a nonzero or unlimited `max_delay` it returns the newest
buffered snapshot at once instead of waiting for an owed lower group. The
`poll_latest` caveat goes. `@moq/net`'s `Ordered` and `@moq/json`'s
`latest()` do the same.

## Waiting

- Maintainer: choose the new public Ordered read's shape and name before
  implementation. Recommend Rust `poll_latest_group` with its async
  `latest_group`, mirrored by JS `tryLatestGroup` and `latestGroup`: select
  the highest eligible buffered sequence across all resume copies in one
  read, pass lower owed holes permanently, and retain the same range,
  delay budget, returned-group guard, and delivered watermark as ordinary
  ordered reads. With nothing eligible, wait for a new candidate or end;
  `next_group` / `nextGroup` keep waiting on owed holes. Alternative:
  `poll_next_available_group` / `next_available_group` and
  `tryNextAvailableGroup` / `nextAvailableGroup` yield the lowest eligible
  group without waiting on holes, leaving snapshot consumers to drain
  them to reach the newest. The direct newest read better expresses the
  goal and avoids handing out every superseded group. Record the choice
  here and remove this wait before implementing.

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

Reproduced 2026-11-03 on the base of
[moq-dev/moq#7736](https://github.com/moq-dev/moq/pull/7736): after reading
snapshot group 0, buffer group 2 while the received track still owes group
1. JS snapshot `latest()` remains pending with `delay.max` of 1,000 ms or
`null`; advancing a manual clock one hour does not release the unlimited
read. Delivering group 1 releases it with state 1, despite group 2 already
being buffered. A two-case scratch probe passed with all pending reads
settled during cleanup. Rust's existing
`next_group_waits_on_an_owed_group_within_the_budget` test also passed,
confirming finite and unlimited owed-hole waits in the Ordered read that
the Rust snapshot consumer drains. No production change is implemented.
