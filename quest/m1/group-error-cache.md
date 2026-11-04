# [M] Group errors are cached by kind, retryable ones for a bounded time

## Goal

Every place moq-net records that a group won't be delivered, in Rust and
JS, follows one rule. A permanent error, where the group can never be
served, is recorded and binds every later reader. A retryable error is kept
only for a bounded time, so a later reader or a widened one asks again once
it lapses, while a burst of readers doesn't re-ask the source for each one.
Both paths that cache `Old` permanently on main follow the rule.

## Plan

Planned 2026-11-04 with the maintainer, while settling
[moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872). In the
maintainer's words: "we should cache errors for some amount of time. I
really hate classifying them though, but I think we kind of have to know
the difference between permanent and retryable errors." Milestone m1
(maintainer).

Where this starts: #7872 (decision 2, 2026-11-04) made a range's own start
skip, a second stale drop, and history recovered once non-permanent.
`Fill::refuse_groups` records nothing, while `Fill::drop_groups` records
the groups as lost for good (`rs/moq-net/src/model/track/fill.rs`). The
only permanent losses it kept are a SUBSCRIBE_DROP NotFound and groups past
the track's end. Two paths on main still cache `Old` permanently, listed as
#7872's follow-ups:

- A live SUBSCRIBE_DROP records every error on the track, `Old` included:
  `self.serving.drop_groups(...)` in `rs/moq-net/src/lite/subscriber.rs`
  (the `SubscribeResponse::Drop` arm), into `track::Producer::drop_groups`
  (`rs/moq-net/src/model/track.rs`).
- `Fill::reject` for lite and IETF FETCH (`reject` in
  `rs/moq-net/src/model/track/fill.rs`) records `Old` as dropped when
  `old_binds_everyone`, that is when the publisher's max age applies to
  every reader.

The work:

- **Inventory** every place a group error is recorded or refused, in Rust
  (`TrackState::drop_groups`, `drop_unfed`, `Fill::drop_groups`,
  `refuse_groups`, `reject_groups`, `reject`, the IETF subscriber's fetch
  and range drops in `rs/moq-net/src/ietf/subscriber.rs`) and JS (`#drop`
  in `js/net/src/track.ts`, `Fill.dropGroups` and `refuseGroups` in
  `js/net/src/fill.ts`, and `js/net/src/ietf/range.ts`). For each, note
  which errors reach it and whether it caches them. Put the inventory in the
  PR.
- **The smallest classification that works.** Recommended starting point:
  permanent for not found, past the track's end, and unauthorized;
  retryable for old, lagged, evicted, cancelled, and unroutable. Keep it a
  property of the error kind, decided in one place, rather than a per-call
  choice. Prefer fewer classes if the inventory shows one is never needed.
  Don't match on error variants at the call sites (AGENTS.md); classify
  once where the error is defined.
- **Bounded caching for retryable errors.** Decide whether a retryable
  error is cached at all, and for how long. Prefer a bound the track
  already has over a new constant: for `Old`, the budget it was judged
  under (a widening already re-asks), or the publisher's max age. The
  bound must also hold up against a hostile peer: what the cache holds is
  sized by the pool, as the [drop set](/quest/m1/drop-set.md) bills its
  runs.
- **Fix both main paths** under the rule: a live SUBSCRIBE_DROP records
  only a permanent error, and `Fill::reject` stops recording `Old` as lost
  for good. Mirror both in JS.
- **To settle in this quest, with the maintainer:** the drop set keeps a
  received stale run with the budget it was judged under, reversed only by a
  widening ("Only a widening reverses a stale verdict"). That is a
  retryable error held without a time bound. Either the bounded-time rule
  covers those runs too, or the drop set's budget scoping already counts as
  the bound for `Old`. Recommended: the latter, so `Old` has one bound (its
  budget) and the time bound covers the other retryable kinds.
- Tests: a retryable refusal is asked again after its bound lapses and not
  before; a permanent one never is; a live SUBSCRIBE_DROP with `Old` no
  longer binds a wider reader; a FETCH answered `Old` under the publisher's
  max age is asked again once the bound lapses.

Public API: possibly a classification query on `Error` (Rust) and its JS
mirror, if a caller outside moq-net needs it; otherwise none. Wire: none.

## Required

- [Drop set](/quest/m1/drop-set.md) - its per-cursor runs and their reasons are what this bounds; its PR, [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670), rewrites the same recording paths
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - its PR, [moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872), makes the start skip and recovery non-permanent, the rule this generalizes

## Related

- [A group a subscriber gives up on is always reported](/quest/m1/group-drops.md) - reports what a subscriber gives up; this decides how long the track remembers it
