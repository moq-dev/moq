# [M] A cancelled moq-transport request stops writing

## Goal

Cancelling a moq-transport SUBSCRIBE, FETCH, TRACK_STATUS, or namespace
request releases its state at once, even when the peer withholds send
credit.

## Plan

Every request write in `rs/moq-net/src/ietf/subscriber.rs` awaits its first
message without watching for cancellation, so a peer that withholds credit
pins a cancelled request.
[IETF lazy discovery](/quest/m1/ietf-lazy-discovery.md) fixes only the
stream-open race. On drafts 16 and up, reset a partially written request
stream. Drafts 14 and 15 share the ordered control stream, so a partial
message cannot be abandoned; decide there whether to finish the message and
cancel right after, and record the choice here.

Test with a peer that never grants credit, on each draft family.

Public API: none. Wire: none.

## Required

- [IETF lazy discovery](/quest/m1/ietf-lazy-discovery.md) - changes the same request writes
