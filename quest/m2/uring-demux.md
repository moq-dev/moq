# [L] One port on the io_uring workers

## Goal

`moq-uring`'s workers host the one-port demux, so a relay on the ring serves
everything the tokio backend does on its one UDP port: QUIC on the worker,
and WebRTC media and SRT reaching their tokio stacks from the same socket.

## Plan

Decided 2026-10-05: the uring workers host the demux. Rejected: keeping
non-QUIC traffic on a separate tokio socket or port on uring nodes, which
reverses the [one-port line](/quest/m2/one-port/README.md)'s point.

Decided 2026-10-05: it ranks in m2 below the bitrate caps, not right after
one-port. The uring rollout is a performance change and the caps are a
customer feature, and moq.pro already ranks one-port above its uring
rollout.

Decided 2026-10-08: the UDP demux shrinks to QUIC plus the WebRTC hook, and
the public STUN responder belongs to the deferred m2 P2P line, so the ring carries no
STUN responder class either. An ICE Binding request is WebRTC traffic and
follows that class.

Each worker's endpoint receives a batch and feeds every segment straight to
the QUIC endpoint. The classifier and flow table from the
[UDP demux](/quest/m2/p2p/udp-demux.md), split there into a
runtime-agnostic piece, run first on each segment; only QUIC continues into
the endpoint. The worker is thread-per-core and `!Send`, while the consumers
of the other classes (`moq-rtc`, `moq-srt`) run on
tokio, so non-QUIC datagrams cross to them on a `Send` channel, and their
replies come back through a remote-send path into the owning worker's ring,
which is what a fed `moq-rtc` takes as its `Transmit`
([WebRTC on the shared socket](/quest/m2/one-port/rtc-feed.md)). A flow
pinned on one shard keeps landing there because reuseport steers only QUIC by
connection ID.

Keep the cross-thread hop off the QUIC path: a QUIC datagram never leaves its
worker. Bound the channel per class with a drop counter, as the tokio demux
does.

Decided in the quest audit: functional parity requires the RTC and SRT feed
implementations, not only packet classification. Reuse their integration
tests through the uring remote-send path with a QUIC session active on the
same port, proving real WebRTC and SRT traffic reaches each stack and its
replies return through the owning worker. An echoed packet can test the
channel but does not prove stack integration. QUIC-bit greasing is already
off on the ring, from shard steering.

## Required

- [UDP demux](/quest/m2/p2p/udp-demux.md) - the classifier and flow table this runs on the ring
- [WebRTC on the shared socket](/quest/m2/one-port/rtc-feed.md) - the fed RTC socket and transmit contract the ring integrates
- [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) - the SRT state machines fed through the shared demux

## Related

- [Stream sessions](/quest/m3/uring-tcp/README.md) - the TCP half of moving the relay onto the ring
