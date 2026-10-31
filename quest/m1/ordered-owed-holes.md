# [M] Ordered reads can skip owed holes, and snapshot latest() does

## Goal

moq-net's `Ordered` gains a read that skips past an owed hole to the newest
buffered group, while keeping the reader's bounds (range, budget, delivered
set). `moq_json::snapshot::Consumer::latest()` moves onto it, so on a
received track with a nonzero or unlimited `max_delay` it returns the newest
buffered snapshot at once instead of waiting for an owed lower group. The
`poll_latest` caveat goes.

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
  or budget, and never hand out a group twice. Mirror it in `@moq/net`'s
  `Ordered` and `@moq/json` only once JS waits on owed groups, which
  [moq-dev/moq#7426](https://github.com/moq-dev/moq/pull/7426) plans as
  `quest/m1/js-ordered-owed.md`.

Public API: adds an `Ordered` read (Rust); `snapshot::Consumer::latest()`
stops waiting at owed holes. Wire: none.

## Required

- [Ordered reads wait only on owed groups](/quest/m2/ordered-owed-wait.md) - [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403) adds the owed-hole wait this read skips
