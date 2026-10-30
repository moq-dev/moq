# [S] Rust answers IETF FETCH as the JS publisher does

## Goal

Rust's IETF publisher (`rs/moq-net/src/ietf/publisher.rs`) matches the JS
publisher from [moq-dev/moq#6945](https://github.com/moq-dev/moq/pull/6945) on
three FETCH behaviors:

1. A joining FETCH that already sent FETCH_OK finishes sending its head even
   if its subscription ends first. Today Rust resets it with the subscription.
2. A joining FETCH whose head group the cache lost is refused with
   DOES_NOT_EXIST before FETCH_OK. Today Rust rejects it with
   `Error::Evicted`, which goes out as INTERNAL_ERROR.
3. A FETCH that opts out of Track Properties (INCLUDE_PROPERTIES `0`) gets
   no TIMESCALE in FETCH_OK and no timestamps on its objects, so the track
   stays untimed for that requester. Today Rust still stamps each object, which
   the requester has no timescale to read.

Non-goals: answering a joining FETCH that arrives after its subscription
ended ([moq-dev/moq#7306](https://github.com/moq-dev/moq/pull/7306)),
and draft-22 fills.

## Plan

Decided 2026-10-27 (maintainer, planning the follow-up of
[moq-dev/moq#6945](https://github.com/moq-dev/moq/pull/6945)): the JS behavior
is the better one on all three, so Rust moves to it rather than JS back.
One quest, since each is a small change to the same publisher, in m1 so both
publishers answer FETCH the same way at the cut.

- **In-flight head.** Once FETCH_OK has gone out, the head group is already
  held and the requester can still cancel, so the FETCH keeps its own
  lifetime instead of the subscription's. It holds nothing a hostile peer
  could not already make it hold through an open FETCH. Check whether this
  is part of the draft-18 `tail` flake with a Rust publisher. That flake was a
  joining FETCH arriving after PUBLISH_DONE, fixed in
  [moq-dev/moq#7306](https://github.com/moq-dev/moq/pull/7306).
- **Lost head.** Refuse with DOES_NOT_EXIST, as a missing prefix already is
  (`a_joining_fetch_refuses_a_missing_prefix_before_fetch_ok`). Update
  `a_joining_fetch_refuses_a_prefix_group_it_lost`.
- **Timestamps.** Only TIMESCALE opts a track into timestamps, so the object
  writer takes the timescale only when Track Properties are wanted.

Test each in `publisher.rs`'s unit tests on mock time. Run
`just test interop --tail --publishers rust --versions ietf` and
`just test interop --all`.

Public API: none. Wire: Rust sends a different refusal code for a lost
head, and omits object timestamps when the requester opted out of Track
Properties.

## Related

- [Headless moq-transport groups](/quest/m2/ietf-headless-group.md) - what a subscriber does with a tail whose head never comes
