# [M] UDP demux

## Goal

`moq-relay` reads one UDP socket and serves QUIC from it. The WebRTC media
path (DTLS, RTP, and ICE's STUN checks) is an embedder hook: the demux hands
its class to an embedder such as moq.pro's edge, which feeds it to
`moq-rtc`, since `moq-relay` serves no WHIP or WHEP.

## Plan

`moq-sock` gains the demux: a task owning the OS socket (or shard) that
classifies each datagram by the rules in the
[questline README](/quest/m2/one-port/README.md) and hands it to the matching
virtual socket, plus the flow table keyed by 4-tuple that WebRTC and SRT pin
into. Keep the classifier and flow table runtime-agnostic, apart from the
tokio task that drives them, because
[the io_uring demux](/quest/m2/uring-demux.md) runs the same piece inside
`moq-uring`'s endpoint. Each virtual socket implements
`AsyncUdpSocket`: `poll_recv` drains its queue, `poll_send` writes through
the shared socket with GSO and ECN passed along, `local_addr` is the shared
one. Bound queues per stack with a per-source drop rather than unbounded
growth; report drops as a counter.

QUIC: `moq-tokio`'s noq server takes the virtual socket through
`new_with_abstract_socket`. QUIC-bit greasing is already off, from
[shard steering](/quest/m2/one-port/shard-steering.md).

STUN: an ICE connectivity check is a Binding request carrying USERNAME, so
it goes to the WebRTC hook, whose mux reads the local ufrag from it. A
Binding request without USERNAME is a public query; drop and count it.
Decided 2026-10-08: the public STUN responder is left to the P2P line
(m3), its only consumer, so `moq-sock` takes no STUN codec
dependency (str0m included).

WebRTC: DTLS, RTP, and USERNAME-carrying STUN go to a `webrtc` hook that
`moq-relay` leaves unconsumed and an embedder takes: the datagrams, a send
path through the shared socket, and a way to pin a 4-tuple to WebRTC in the
flow table and release it. [WebRTC on the shared socket](/quest/m2/one-port/rtc-feed.md)
makes `moq-rtc` consume it.

Decided in the 2026-09-30 audit: the relay itself serves only QUIC here,
because `moq-relay` has no `moq-rtc` dependency and serves no WHIP or WHEP.
Decided 2026-10-05: feeding `moq-rtc` moved from the embedder to
rtc-feed, because `moq-rtc` is an upstream, generic crate.

Tests: a unit test per first-byte class routes to the right virtual socket,
including the WebRTC hook; an integration test runs a QUIC client against
the demuxed port. `moq-relay` docs list the port once.

## Required

- [Steer only QUIC by connection ID](/quest/m2/one-port/shard-steering.md) - the flow table is per shard, so each non-QUIC flow has to stay on one

## Related

- [P2P](/quest/m3/p2p/README.md) - the only consumer of a public STUN responder, which it plans when it needs one
- [One port on the io_uring workers](/quest/m2/uring-demux.md) - runs this classifier and flow table on the io_uring workers
