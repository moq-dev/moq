# [L] UDP demux

## Goal

`moq-relay` reads one UDP socket and serves QUIC and STUN Binding answers from
it. A P2P client can list `stun:<relay>:<port>`, and every backend config has
QUIC-bit greasing off so a short header is always recognizable. The WebRTC
media path is an embedder hook: the demux hands its class to a virtual socket
an embedder such as moq.pro's edge can feed to `moq-rtc`, since `moq-relay`
serves no WHIP or WHEP.

## Plan

`moq-sock` gains the demux: a task owning the OS socket (or shard) that
classifies each datagram by the rules in the
[questline README](/quest/m2/one-port/README.md) and hands it to the matching
virtual socket, plus the flow table keyed by 4-tuple that SRT will need,
built now so its shape is settled. Each virtual socket implements
`AsyncUdpSocket`: `poll_recv` drains its queue, `poll_send` writes through
the shared socket with GSO and ECN passed along, `local_addr` is the shared
one. Bound queues per stack with a per-source drop rather than unbounded
growth; report drops as a counter.

QUIC: `moq-tokio`'s noq server takes the virtual socket through
`new_with_abstract_socket`. Set the grease-QUIC-bit transport parameter off,
with a test that decodes a sent short-header
packet and asserts the fixed bit.

STUN: a `stun` virtual socket answered by a small responder in `moq-sock`,
Binding request to Binding success with XOR-MAPPED-ADDRESS, str0m's
`StunMessage` or a maintained crate for the codec, behind a per-source token
bucket and a global budget since the answer is up to 2.2 times the request.
The per-source table is fixed-capacity with expiry, or a hashed/stateless
limiter; spoofed sources cannot grow it.
An ICE connectivity check is also a Binding request, so the STUN class splits
on the USERNAME attribute: a request carrying one belongs to a WebRTC session
and goes to the mux, which reads the local ufrag from it; a request without
one is a public query and goes to the responder. Public STUN clients never
send USERNAME and ICE agents always do.
Off by default in `moq-relay`, on with `--stun`.

WebRTC: DTLS, RTP, and USERNAME-carrying STUN go to a `webrtc` virtual
socket that `moq-relay` leaves unconsumed and an embedder takes. Adapting
`moq_rtc::server::mux::Mux` to it (`Mux::feed`, a shared advertised address,
and pinning ICE 4-tuples in the flow table) is the embedder's work, not this
quest's.

Decided in the 2026-09-30 audit: narrowed to QUIC plus STUN in the relay,
because `moq-relay` has no `moq-rtc` dependency and serves no WHIP or WHEP.

Tests: a unit test per first-byte class routes to the right virtual socket,
including the WebRTC hook; an integration test runs a QUIC client and a STUN
Binding round trip against one bound port. `moq-relay` docs list the port once.

## Related

- [P2P](/quest/m3/p2p/README.md) - the client side of the STUN answer
