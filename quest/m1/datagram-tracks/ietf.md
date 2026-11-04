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

Rust datagram tracks ([moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752))
left moq-transport without datagrams in Rust until this lands: the IETF
subscriber drops every OBJECT_DATAGRAM, `datagrams()` on an IETF source is
NOT_FOUND, and a stream track never sends one. Restore what it removed:

- the Rust-JS `datagram` tail lanes in `test/interop/interop.sh`, the
  `datagram` mode of `test/interop/clients/js-native/tail.ts`, and their
  README bullet;
- moq-e2ee's `datagrams_over_ietf` transport test;
- the moq-transport rows of moq-tokio's `datagram_delivery` bench and
  moq-net's `session_delivery_datagram`;
- the OBJECT_DATAGRAM sentence in `doc/concept/standard.md`.

Public API: none beyond the model. Wire: no format change.

## Required

- [JS datagram tracks](/quest/m1/datagram-tracks/js.md) - the JS datagram types this maps
