# [M] SRT on the shared socket

## Goal

`moq-srt` serves from the demuxed UDP socket beside QUIC, so an SRT encoder
dials the relay's one port and the flow table pins its 4-tuple after the
induction handshake. An SRT ingest whose peer goes silent aborts its
broadcast instead of finishing it as if the peer had shut down.

## Plan

`srt-tokio` 0.4 takes a `tokio::net::UdpSocket` in `bind_with_socket` and
nothing more abstract. Decided 2026-09-30: `srt-protocol` is sans-io, so
`moq-srt` drives its `Listen` and `Connection` state machines directly on the
demuxed packets. That is more code, but it removes the dependency on
`srt-tokio`'s socket handling. An upstream socket trait was rejected:
upstream (now rosalyntg/srt-rs) has not been pushed since 2024-05, and its
maintainer said they do not actively support it.

The demux side is the flow table from [UDP demux](/quest/m2/p2p/udp-demux.md):
a 4-tuple already pinned to WebRTC (ICE succeeded) is never tested for SRT.
An unknown 4-tuple whose full SRT handshake header matches (control bit,
type 0, and the SRT magic, not merely `80 00`) pins to SRT provisionally:
the provisional table is small and bounded, per-source rate limited, and an
entry lives only a few seconds unless the SRT stack reports the conclusion
handshake complete, at which point it is promoted and routes there until
the SRT peer idle timeout. A spoofed flood therefore fills a fixed table
for seconds, not the idle timeout. Add the induction-packet check with a
test vector from a real encoder, a vector that an RTP PT0 packet is not
induction, and a test that floods provisional entries without evicting an
established one.

The driver also tells a clean end from a failure, in both the server and
the `Client` pull (`serve_publish`): a close without a valid peer Shutdown
control packet aborts the broadcast, and a clean shutdown finishes it.
Decided 2026-10-24 to land it here rather than in m1: srt-tokio 0.4.4 ends
its stream with `None` for a peer Shutdown, the peer idle timeout, and a
socket error alike, and srt-protocol keeps no close cause, so only a driver
that sees every inbound packet can tell. Test both with a mocked clock,
which the driver allows by taking `now` as input; srt-tokio reads std's
clock, so `tokio::time::pause` cannot drive its idle timeout.

Tests: `moq-srt`'s existing integration test runs against the shared socket
with a QUIC client active on the same port; a QUIC short header from a new
4-tuple during an SRT session still reaches QUIC.

## Required

- [UDP demux](/quest/m2/p2p/udp-demux.md)
