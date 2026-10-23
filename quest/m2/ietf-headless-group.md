# [M] Keep a moq-transport group whose head never arrives

## Goal

When a moq-transport subscription delivers a group starting partway through
and the fill or joining FETCH that should supply its head is refused or
ends empty, moq-net keeps the tail as a group that starts at that object.
Today, depending on a race, it either drops the whole group or keeps the
tail as if it were the whole group. A relay then forwards the tail to its
moq-transport subscribers instead of losing objects it received, which
draft-18 §9.4 forbids ("A relay MUST NOT reorder or drop objects received
on a multi-object stream").

## Plan

Decided with the maintainer on 2026-10-22, from the runner cases work
([moq-dev/moq#6275](https://github.com/moq-dev/moq/pull/6275)): keep the
tail, don't drop the group. This was ranked m2 because it blocks no m1
interop goal: a publisher that answers SUBSCRIBE_OK before writing, or that
serves the joining FETCH, never triggers it.

Where it happens: `open_group` in `rs/moq-net/src/ietf/subscriber.rs`.
The outcome is a race between the mid-group stream and the joining FETCH's
refusal:

- If the refusal lands first, the fill is no longer outstanding, and the
  stream takes the unfilled path. That keeps the tail, but only the wire
  metadata (`group::Wire.first_id`) records where it starts: the model
  group begins at frame 0, so it looks complete, and `position_group`
  serves the tail to moq-lite subscribers as a whole group.
- If the stream arrives while the fill is still pending, it waits in
  `claim_fill`. Once the refusal settles the fill, `claim` returns no head,
  and the stream fails with "no fill to stitch a mid-group stream onto",
  which drops the whole group.

Cloudflare's moq-test-client reproduces both: it writes object 0 before
SUBSCRIBE_OK and refuses the joining FETCH
([cloudflare/moq-rs#281](https://github.com/cloudflare/moq-rs/issues/281)).

The ingest fix opens the same headless group in both orders, and the model
must know its head is missing so `position_group` refuses it to moq-lite
subscribers. `group::Producer::start_at` marks a missing front, but in
frame ordinals: relayed Object IDs need not match them (`group::Wire`), so
trimming to the first Object ID would invent frames and break FETCH and
partial-reader positioning. Mark the missing head without equating the
two, and check that the moq-transport egress forwards the group as a
subgroup stream starting at its first Object ID.

Open design points, with recommendations:

1. **Serving the tail to moq-transport subscribers.** Recommended: forward
   it as a subgroup stream that starts at its first object, since §9.4
   applies. The alternative is to skip it, as moq-lite does.
2. **Serving the tail to moq-lite subscribers.** Recommended: keep the
   current DROP (`position_group` in `rs/moq-net/src/lite/publisher.rs`)
   for any subscriber that did not ask for a partial group. On moq-lite a
   group is the unit of decodability, and only the subscriber knows whether
   a partial one is any use.
3. **A later FETCH or joining FETCH for the missing head.** Recommended:
   refuse the missing prefix, as the publisher already does
   (`a_joining_fetch_refuses_a_missing_prefix_before_fetch_ok`), rather
   than ask upstream again.

Tests: a subscriber unit test for each order, with the fill refused before
the mid-group stream arrives and after it. The two orders must end the same
way. Add a relay-level test that a moq-transport
subscriber receives the tail while a moq-lite subscriber gets the DROP.

Public API: none expected. Wire: none.

## Related

- [Late join and FETCH](/quest/m1/interop-coverage/late-join.md) - the
  joining FETCH that normally supplies the head
