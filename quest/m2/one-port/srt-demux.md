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

The demux side is the flow table from [UDP demux](/quest/m2/one-port/udp-demux.md):
an unknown 4-tuple whose first byte is 0x80 with control type 0 pins to SRT,
and a pinned 4-tuple routes there until it goes idle for the SRT peer idle
timeout. Add the induction-packet check with a test vector from a real
encoder.

Tests: `moq-srt`'s existing integration test runs against the shared socket
with a QUIC client active on the same port; a QUIC short header from a new
4-tuple during an SRT session still reaches QUIC.

## Required

- [UDP demux](/quest/m2/one-port/udp-demux.md)
- srt-tokio accepts a caller-supplied socket abstraction upstream, or the sans-io fallback is chosen
