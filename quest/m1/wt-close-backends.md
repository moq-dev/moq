# [S] iroh and moq-uring deliver a peer's close

## Goal

`web-transport-iroh`'s client reads a peer's capsule close, so it surfaces as
`(0, "stream closed")`, and moq-uring's HTTP/3 close task keeps the session
alive structurally until the close capsule is sent. Both are proven by a
regression test.

## Plan

`just test interop` already proves the browser path for the relay moq ships
(`web-transport-moq`): its "browser close code" case fails on the release before
[moq-dev/noq#23](https://github.com/moq-dev/noq/pull/23).

Decided 2026-10-06: `web-transport-noq` and `web-transport-quinn` are out of
scope. moq consumes neither, moq-dev/noq is frozen to security patches, and
moq's own QUIC moves to `moq-quic` ([Own the QUIC stack](/quest/m1/quic/README.md)).
Their close-capsule bug only affects third-party users.

- `web-transport-iroh` keeps sending no capsule, only a QUIC close (decided with
  the maintainer: browsers cannot dial an iroh endpoint). Fix its client, which
  reads capsules without the HTTP/3 DATA framing the other backends send. That
  work lands in moq-dev/web-transport, followed by a pin bump here.
- moq-uring's own HTTP/3 close task holds only the connection and send stream;
  it survives only because the capsule reader keeps the session state alive.
  Make that structural.

Public API: none. Wire: none.
