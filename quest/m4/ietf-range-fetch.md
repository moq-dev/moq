# [M] moq-transport FETCH answers ranges over known holes

## Goal

A Rust moq-transport publisher answers a Standalone `FETCH` (and a draft 20+
fill) that spans more than one group, provided the cache knows the status of
every group in the range: each one is either cached or proven never
produced. Groups go out in order on the one fetch stream, and each proven
hole is marked with End of Non-Existent Range (`0x8C`). A range with any
unknown group is refused whole, as today. A moqx edge in front of a
moq-relay origin, and two moq-net relays chained over moq-transport, can
then replay history in one request.

Not here: a hole-marker extension of our own (deleted under the IETF scope
freeze), hole knowledge for copies fed by peers that send no gap, and JS,
whose publisher refuses every `FETCH`.

## Plan

Decided 2026-11-12 (maintainer, `/quest-import` of
[moq-dev/moq#8577](https://github.com/moq-dev/moq/issues/8577)):
✅ a narrow quest in m4 (rejected: won't fix under the freeze; the same in
m3 for the reporter as a named consumer; the proposal as written, which walks
every ID and resets with `UNKNOWN_OBJECT_STATUS`). This narrows
[moq-dev/moq#7204](https://github.com/moq-dev/moq/pull/7204)'s one-group
refusal, whose reason was that "serving moq-transport ranges requires
authoritative knowledge of every group". This quest serves only ranges
where we have that knowledge. It widens the
frozen scope `doc/concept/standard.md` states, which kept only joining
`FETCH`.

What makes it possible: moq-transport's Prior Group ID Gap object extension,
which [IETF time addressing](/quest/m2/time-addressing/ietf.md) sends on
egress and reads on ingest as a best-effort SUBSCRIBE_SKIP. What counts as
proven:

- a group in the cache;
- a hole the copy knows was never produced: local, moq-lite SUBSCRIBE_SKIP,
  or a received Prior Group ID Gap;
- a budget-independent drop (a cut, or an origin's eviction or expiry).

Anything else is unknown, and refuses the range: a drop judged under a
narrower budget or range, which proves nothing for a wider `FETCH`; a gap
from a moq-transport peer that sends no Prior Group ID Gap (its absence is
not "no gap"); and a dropped or expired group from a moq-transport upstream,
which has no marker for it.

- Both refusals in `rs/moq-net/src/ietf/publisher.rs` (Standalone `FETCH`
  admission, and `judge` for the draft 20+ forms) answer instead when the
  range is fully known. `FETCH_OK`'s End Location is cut at the largest
  object, and a start past it stays `INVALID_RANGE`.
- Answer through the existing per-group path in group order, fetching the
  next group only when the stream has room, and stop when the requester
  cancels. Bound it the way one-group fetches are bounded; the relay keeps
  any limit on groups per `FETCH`.
- The `track::Requests` handler shape stays: a `Fill` already separates gone
  (`drop_groups`) from unknown (`refuse_groups`). Answer from what the copy
  records, without asking upstream group by group.
- Update the "History is served one group at a time" entry in
  `doc/concept/standard.md`, and the scope paragraph above it, which
  names a multi-group `FETCH` as refused.

Test on drafts 18, 20, and 22: a range over cached groups and proven holes is
answered with `0x8C` runs; a range with one unknown group is refused; an
upstream that reports G stale for a narrow subscriber while still holding G
gets G or a refusal for a wider `FETCH` covering G, never `0x8C`; and a chain
of two moq-net relays over moq-transport replays a multi-group run. Run the
interop suite with `--all`.

Public API: none. Wire: none (draft-defined messages only).

## Required

- [IETF time addressing](/quest/m2/time-addressing/ietf.md) - reads Prior Group ID Gap on ingest

## Closes

- [#8577](https://github.com/moq-dev/moq/issues/8577) - a Standalone FETCH that spans more than one group is refused
