# [S] moq-transport FETCH and fill streams in the session queue

## Goal

Under congestion, a moq-transport FETCH or fill stream competes with the
session's group streams through the same priority queue, ranked by its
subscriber priority and the group it carries. Today the Rust moq-transport
publisher gives these streams the raw subscriber priority as their send order,
so they fall below every ranked group stream whatever their priority.

## Plan

Where it stands after #5385 (`rs/moq-net/src/ietf/publisher.rs`):

- Group streams take a `Rank` from the session `PriorityQueue` through
  `TrackServe::rank(sequence)`, keyed by the subscription's request id and
  ordered by its `Order`.
- `run_fill` calls `stream.set_priority(priority)` with the raw subscriber
  priority, and so does the FETCH stream in `run_fetch_stream`.

The work:

- A draft-20 fill joins its subscription's ranking as the group it fills,
  the way JS already ranks it (`js/net/src/ietf/publisher.ts`, `#runFill`).
  A joining FETCH, used before draft-20, ranks the same way within the
  subscription it joins.
- A standalone FETCH gets its own queue entry: its subscriber priority, its
  own request id, and its group, ordered by the FETCH's GROUP_ORDER. It
  follows priority changes the way a group stream's `Rank` does.
- JS parity (decided 2026-10-13 in #5400). JS already ranks its fill through
  the subscription's `Priority`. It serves no standalone FETCH: `runFetch`
  (`js/net/src/ietf/publisher.ts`) refuses every one. So JS has nothing to
  change now, and any FETCH serving it gains later ranks through the same
  `Priority`.
- Keep each stream ranked until it settles, not just until its FIN is
  written (the #5385 review fix for JS group streams).
- Tests, on mock time, in Rust: a fill and a FETCH stream ranked against open
  group streams of the same and a different priority, asserting
  the recorded send orders the way `group_stream_preserves_model_priority`
  does.

This is unblocked and lands before [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md),
which makes a FETCH span several groups. That quest then advances a FETCH
stream's entry as it moves from group to group.

Out of scope: lite FETCH. lite-07 removes it, and published lite versions
answer FETCH through the range model that [Model ranges](/quest/m1/subscribe-ranges/model.md)
builds.

## Related

- [Scope and sign track priority](/quest/m1/track-priority-scope.md) - changes what the queue compares across subscriptions
- [FETCH max-delay](/quest/m1/fetch-max-delay.md) - the same FETCH serving path
