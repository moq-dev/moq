# One port

## Goal

A relay, or a binary embedding the gateway crates beside one, speaks
everything it serves on one UDP port and one TCP port. UDP carries QUIC
(WebTransport and raw moq), the WebRTC media path (STUN, DTLS, SRTP) for
WHIP and WHEP, and SRT. TCP carries TLS-terminated
HTTP (WebSocket qmux, WHIP and WHEP signaling, HLS, ops), `tls://` qmux,
RTMP, and RTMPS.
Upstream delivers the demux and stacks that accept a fed socket or stream;
`moq-relay` itself serves QUIC and HTTP on them,
and an embedder such as moq.pro's edge wires WebRTC, RTMP, and SRT, which the
relay binary has never spoken. An operator opens 443 twice and is done; a client on
a network that permits only 443 reaches every protocol.

The demux is a `moq-sock` primitive over the tokio backends.
[`moq-uring`'s workers](/quest/m2/uring-demux.md) host it later; that is not
a blocker for this line.

## Plan

Deferred to m2 in the 2026-09-30 audit: the consumer is moq.pro's edge, and
STUN's only consumer is P2P, deferred to m2. SRT is not blocked upstream:
[SRT demux](/quest/m2/one-port/srt-demux.md) decided on 2026-09-30 to drive
sans-io `srt-protocol` directly instead of waiting on a socket abstraction in
srt-tokio.

Decided 2026-10-05, from moq.pro's audit: `moq-rtc` taking a fed socket
([rtc-feed](/quest/m2/one-port/rtc-feed.md)) joins this line, and
[the io_uring demux](/quest/m2/uring-demux.md) is planned beside it, all in
m2 rather than m3, since moq.pro's edge cutover and its uring rollout wait on
them.

The UDP demux has landed: `moq_sock::demux` classifies, and the tokio QUIC
listener's socket diverts the WebRTC media path inline in its receive to a
`moq_tokio::quic::demux::WebRtc` feed, which `moq_relay::Relay::webrtc`
hands an embedder. [Native pairs](/quest/m2/p2p/native-quic.md) reuse the
classifier. QUIC shard steering landed in #5638; reuse its filter rather than
recreating that completed work.

Decided 2026-10-31 by the maintainer, in
[moq-dev/moq#7095](https://github.com/moq-dev/moq/pull/7095): the split runs
inline in the QUIC socket's `poll_recv`, not in a task owning the socket and
fanning out to per-stack virtual sockets, so a QUIC datagram keeps its
zero-copy, same-task path and its GRO batch. The API stays as shipped:
`moq_tokio::quic::demux::Demux` in `server::Config::demux`, `Demux::webrtc()`
returning the `WebRtc` feed, and `moq_relay::Relay::webrtc()`. SRT adds its
class to the same inline split.

### Classifying a datagram

RFC 7983 already partitions the first byte: STUN is 0 to 3, DTLS 20 to 63,
RTP and RTCP 128 to 191. QUIC fills the gaps: a long header is 192 to 255 and
a short header 64 to 127, as long as the fixed bit is set, so the QUIC
config disables QUIC-bit greasing (RFC 9287) explicitly. `moq-quic` turns it
on by default, but the existing shard-steering path already disables it in
`rs/moq-tokio/src/quic/backend.rs`. The shared demux must apply the same
requirement to every QUIC endpoint it serves, including an unsharded one.
SRT does not fit:
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

One OS socket, or one shard of a reuseport group, is read by the QUIC
endpoint's own socket, which keeps QUIC in place and copies every other
class into that stack's bounded feed, shared by every shard. Sends go
straight to the shared socket, so every stack answers from the same address
and port. `moq-rtc`'s `Mux` already demuxes STUN by ufrag
internally and takes a fed socket in
[WebRTC on the shared socket](/quest/m2/one-port/rtc-feed.md). SRT skips `srt-tokio`,
which accepts a `tokio::net::UdpSocket` but no abstraction, and feeds
datagrams to sans-io `srt-protocol` as [SRT demux](/quest/m2/one-port/srt-demux.md)
decided.

### STUN

Decided 2026-10-08: the public STUN responder is left to P2P (m2), its
only consumer. Here, a Binding request carrying USERNAME is an ICE check
and goes to WebRTC, as does every Binding response, since the WebRTC agent
runs full ICE and only it sends requests from this port. Any other STUN is
dropped and counted. Decided 2026-10-13:
the responder binds its own port ([relay STUN](/doc/bin/relay/config.md#stun)),
so the shared port never answers public Binding requests.

## Required

- [WebRTC on the shared socket](/quest/m2/one-port/rtc-feed.md) - `moq-rtc` serves WHIP and WHEP media from the WebRTC hook and pins ICE tuples
- [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) - moq-srt drives `srt-protocol` on demuxed packets and the flow table pins its 4-tuples

## Related

- [One port on the io_uring workers](/quest/m2/uring-demux.md) - the io_uring workers host the UDP demux
- [Stream transports](/quest/m2/perf/stream/README.md) - the io_uring workers that would host the TCP acceptor later
