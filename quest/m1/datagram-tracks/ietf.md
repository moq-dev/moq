# [S] moq-transport datagram tracks

## Goal

Over moq-transport, Rust and JS map a datagram track to IETF SUBSCRIBE and
OBJECT_DATAGRAM, with its sequence as the group ID and object ID 0, and
keep the requested kind when a peer mixes datagrams and streams on one track.

## Plan

Decided 2026-11-01 (maintainer), in the
[epic](/quest/m1/datagram-tracks/README.md):

- A datagram-track subscription keeps only datagrams with object ID 0.
  Stream objects arriving on it are dropped and logged once per
  subscription.
- A stream-track subscription drops datagrams.
- Outbound, a datagram track sends only OBJECT_DATAGRAM with object ID 0.
  A stream track never sends OBJECT_DATAGRAM.
- A datagram with a nonzero object ID on a datagram-track subscription is
  dropped and logged.

Touches `rs/moq-net/src/ietf/datagram.rs` and the JS IETF adapter. Test
against moxygen in the interop lanes where it sends datagrams.

Public API: none beyond the model. Wire: no format change.

## Required

- [Rust datagram tracks](/quest/m1/datagram-tracks/rust.md) - the datagram types this maps
- [JS datagram tracks](/quest/m1/datagram-tracks/js.md) - the JS datagram types this maps
