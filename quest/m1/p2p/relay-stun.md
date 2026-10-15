# [S] Relay STUN

## Goal

`moq-relay` answers public STUN Binding requests on a `[stun] bind` UDP
address, so an application can name its relay as the STUN server its P2P
peers use. Unset is off. Clients use it only when the application lists it
among its ICE servers (`stun:<relay host>:<port>`); nothing adds it
automatically.

## Plan

Decided with the maintainer on 2026-10-13:

- Plan it now. The earlier gate (wait until the harness shows the relay's
  STUN beats the application's servers) is dropped: P2P uses STUN and ICE
  through the public relay CDN rather than iroh.
- Its own port, not the QUIC port. It stays independent of the
  [UDP demux](/quest/m1/p2p/udp-demux.md), shard steering, and QUIC-bit
  greasing, and a NAT's server-reflexive mapping does not depend on which
  server port is asked.
- Applications opt in explicitly; clients never infer it from the relay
  session.

Answer a Binding request with XOR-MAPPED-ADDRESS only. The reply outsizes
the request (32 or 44 bytes against 20), so bound spoofed amplification with
a per-source token bucket and a global budget, and count drops. Use the
`is` crate's STUN codec (already in the lockfile through str0m), not a
hand-rolled one. Refuse anything other than a Binding request.

Docs: a `[stun]` section in `doc/bin/relay/config.md`, and a line in the
P2P setup in the [routing guide](/quest/m1/routing-guide.md) on listing the
relay as an ICE server.

Tests: a Binding request gets the sender's mapped address; a malformed or
non-Binding request gets nothing; the token buckets drop past budget, with
mocked time.

## Related

- [P2P](/quest/m1/p2p/README.md) - its only consumer
- [One port](/quest/m2/one-port/README.md) - the shared port drops public Binding requests instead
