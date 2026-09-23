# [M] SRT on the shared socket

## Goal

`moq-srt` serves from the demuxed UDP socket beside QUIC, so an SRT encoder
dials the relay's one port and the flow table pins its 4-tuple after the
induction handshake.

## Plan

`srt-tokio` 0.4 takes a `tokio::net::UdpSocket` in `bind_with_socket` and
nothing more abstract. Preferred: an upstream PR giving `SrtListener` a
socket trait (`poll_recv_from` and `poll_send_to`) that a real socket and our
virtual socket both implement, then `moq_srt::Server` takes the virtual
socket. Fallback if refused: `srt-protocol` is sans-io, so `moq-srt` drives
its `Listen` and `Connection` state machines directly on fed packets, which
is more code but removes the dependency on `srt-tokio`'s socket handling.

The demux side is the flow table from [UDP demux](/quest/m1/one-port/udp-demux.md):
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

Tests: `moq-srt`'s existing integration test runs against the shared socket
with a QUIC client active on the same port; a QUIC short header from a new
4-tuple during an SRT session still reaches QUIC.

## Required

- [UDP demux](/quest/m1/one-port/udp-demux.md)
- srt-tokio accepts a caller-supplied socket abstraction upstream, or the sans-io fallback is chosen
