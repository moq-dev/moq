# [S] Datagrams are live-only

## Goal

A datagram group is delivered only to subscribers present when it is sent.
FETCH never returns one, a new subscription never replays buffered datagrams,
and a relay fills no cache miss with one. Anything that needs reliable or
late-joiner delivery should use a stream group instead.

## Plan

Decided in planning (09-29), after the moxygen fetch-fill agent found that the
relay's group-fetch decoder refuses a draft-16+ fetch object that carries the
datagram flag, and reports it as "unsupported":

- Keep the existing 64-datagram send buffer per track
  (`rs/moq-net/src/model/track.rs`, `MAX_DATAGRAMS`). An age bound was
  considered and dropped.
- A new subscription starts at the next datagram sent, not at the buffered
  backlog. This covers Rust and `@moq/net`, lite and IETF.
- FETCH never serves a datagram group. A publisher answers a FETCH for one the
  way it answers any group it won't deliver (dropped or does not exist),
  never with its payload.
- A received fetch object with the datagram flag is refused as not fetchable.
  It fails only that fetch or fill, not the session, because draft-16+ allows
  the flag (maintainer, 09-29, on Codex's review). That covers the relay's group
  fill (`recv_group_fetch_objects`) and the joining-fetch
  fill (`run_fill_objects`).
- Update `drafts/draft-lcurley-moq-lite.md` to say datagrams are neither
  cached nor fetchable, and `doc/concept/`. Run `just drafts check` and
  `just test interop --all`.

Not planned: reliable or cached datagrams. If they are ever wanted, the way to
get them is back-pressure in the QUIC library and treating each datagram like
a one-shot stream, not a cache.

The subscription's range bounds datagrams the way it bounds groups, fixed in
the model rather than filtered per session. Decided in the 2026-09-30 audit:
the moxygen line's datagram-range quest merged here, since it duplicated this
late-join rule. Its edge cases:

- a datagram at the start group when a frame offset skips object 0;
- SUBSCRIBE_UPDATE moving the range while datagrams are in flight;
- a datagram that lands before the subscription's alias or id is known.

Tests: a subscriber joining after datagrams were sent receives only later
ones. A FETCH covering a datagram group gets no payload. A fetch stream
carrying a datagram-flagged object fails that fetch and leaves the session up.
Each edge case above gets one, and a test tells a datagram filtered by the
range apart from one dropped for any other reason.

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - datagram groups stay best effort there too
