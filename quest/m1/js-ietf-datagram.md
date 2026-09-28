# [M] @moq/net datagrams over moq-transport

## Goal

A `@moq/net` session on moq-transport sends and receives datagram groups as
`OBJECT_DATAGRAM`, one object per group with its sequence kept, as Rust does.
A datagram track crosses IETF between Rust and JS in both directions in
`just test interop`.

## Plan

- JS routes datagrams on moq-lite only (`js/net/src/lite/datagram.ts`,
  `runDatagrams` from lite-05); `js/net/src/ietf/` reads and writes none.
  Mirror the lite path on the IETF session and the Rust mapping from #4274:
  receive decodes `OBJECT_DATAGRAM` and inserts on the aliased subscription's
  track; send writes object 0 with END_OF_GROUP, the explicit publisher
  priority, and the timestamp property when the track has a timescale.
- Keep Rust's edges: an Object ID other than 0, a non-Normal status, or an
  unbound alias is dropped; a malformed Type closes the session. Rust covers
  drafts 14 and later, whose Type flags differ between 14 and 15+; decide
  what draft 07, which JS also speaks, does.
- Add IETF datagram cases beside the lite ones in the interop harness.

Public API: none expected. Wire: `@moq/net` moq-transport sessions send and
accept `OBJECT_DATAGRAM` as the drafts define; no project draft changes.

## Required

- [Datagram groups](/quest/m1/moxygen/datagram.md) - the Rust side this mirrors and interops with
