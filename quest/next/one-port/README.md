# One port

## Goal

A relay, or a binary embedding the gateway crates beside one, speaks
everything it serves on one UDP port and one TCP port. UDP carries QUIC
(WebTransport and raw moq), STUN Binding answers, the WebRTC media path
(STUN, DTLS, SRTP) for WHIP and WHEP, and SRT. TCP carries TLS-terminated
HTTP (WebSocket qmux, WHIP and WHEP signaling, HLS, ops), RTMP, and RTMPS.
Upstream delivers the demux, the responder, and stacks that accept a fed
socket or stream; `moq-relay` itself serves QUIC, STUN, and HTTP on them,
and an embedder such as moq.pro's edge wires RTMP and SRT, which the relay
binary has never spoken. An operator opens 443 twice and is done; a client on
a network that permits only 443 reaches every protocol; a P2P client names
the relay as its STUN server and gets the lowest-RTT reflexive candidate
there is.

The demux is a `moq-sock` primitive over the tokio backends. `moq-uring`'s
reuseport shard groups are a later consumer, not a blocker.

## Plan

### Classifying a datagram

RFC 7983 already partitions the first byte: STUN is 0 to 3, DTLS 20 to 63,
RTP and RTCP 128 to 191. QUIC fills the gaps: a long header is 192 to 255 and
a short header 64 to 127, as long as the fixed bit is set, so the QUIC
config disables QUIC-bit greasing (RFC 9287) explicitly; noq turns it on by
default and nothing here disables it today. SRT does not fit:
its data packets start with a 0 bit and its control packets with a 1, so both
overlap. SRT is demuxed by flow instead. A 4-tuple ICE has succeeded on is pinned
to WebRTC in the outer table before any SRT test, because an RTP v2 packet
with marker 0 and payload type 0 begins `80 00`, the same two bytes as a
naive SRT induction check. SRT induction is classified only for still-
unknown tuples, and the check is the full handshake header (control bit,
type 0, and the SRT magic), not the first two bytes. Every later packet
from a pinned 4-tuple follows that pin regardless of byte. Anything else
from an unknown 4-tuple with a QUIC-shaped first byte is QUIC, which
handles its own migration by connection id. An RTP-shaped packet from an
unknown tuple is not SRT; it is dropped or given to the WebRTC mux.

### Virtual sockets

One OS socket, or one shard of a reuseport group, is read by the demux and
fanned into virtual sockets, one per stack, each with the `AsyncUdpSocket`
shape. Sends go straight to the shared socket, so every stack answers from
the same address and port. noq accepts the virtual socket through
`new_with_abstract_socket`. `moq-rtc`'s `Mux` already demuxes STUN by ufrag internally and only
needs a `feed` entry beside its `recv_from` loop. `srt-tokio` accepts a
`tokio::net::UdpSocket` but no abstraction; that is its own quest.

### STUN

A Binding request is answered with a Binding success carrying
XOR-MAPPED-ADDRESS, no authentication, no other methods. The response is
larger than a minimal request (32 or 44 bytes against 20), so a spoofed
source is a small amplifier; a per-source token bucket and a global responder
budget bound it, and the drop counter makes it visible. Use str0m's
`StunMessage`, already a dependency, or a maintained STUN crate; do not
hand-roll the codec.

### TCP

The 1935 listener downstream already peeks one byte to split RTMPS from RTMP.
The unified acceptor generalizes it: 0x16 is TLS, terminated here, then the
first decrypted byte is peeked again; 0x03 is an RTMP handshake and anything
else is HTTP, which goes to the axum router. A plaintext 0x03 is RTMP and a
plaintext ASCII method is HTTP. RTMPS clients send no ALPN, so ALPN cannot do
this. The acceptor yields classified connections; `moq-rtmp`'s
`accept_stream` already takes any `AsyncRead + AsyncWrite`, and
`axum_server::Server::from_listener` takes a listener that a channel of
pre-accepted streams can stand behind.

## Quests

- [UDP demux](/quest/next/one-port/udp-demux.md) - one socket carries QUIC, STUN answers, and the WebRTC media path, with greasing off
- [TCP acceptor](/quest/next/one-port/tcp-demux.md) - one listener carries TLS-terminated HTTP, RTMP, and RTMPS
- [SRT on the shared socket](/quest/next/one-port/srt-demux.md) - srt-tokio accepts a virtual socket and the flow table pins its 4-tuples

## Related

- [P2P](/quest/next/p2p/README.md) - the client that names the relay as its STUN server
- [Stream sessions](/quest/next/uring-tcp/README.md) - the io_uring workers that would host the same demux later
