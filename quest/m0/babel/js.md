# [S] JS codec

## Goal

`@moq/net` decodes and encodes the lite-07 route fields from the
[Babel routing](/quest/m0/babel/README.md) line. As a publisher it answers
ANNOUNCE_REFRESH by bumping the route's seqno, and it tells a reroute from a
takeover without reading a hop list.

## Plan

- `js/net/src/lite/`: ANNOUNCE_START/UPDATE fields and ANNOUNCE_REFRESH.
- Every connection mints a random hop today (`lite/connection.ts`). The source
  id is per origin instead, so a reconnecting browser publisher keeps its
  identity.
- `lite/subscriber.ts` separates a reroute from a takeover with
  `hops[0] ?? responderOrigin`. Key it on the source id, or on the reply's
  origin once Spread lands.
- The tie-break on hop count in `origin.ts` goes away.
- Interop: `just test interop --all` against the Rust side.

## Required

- [Rust routing](/quest/m0/babel/rust.md) - interop needs the Rust side of the wire
