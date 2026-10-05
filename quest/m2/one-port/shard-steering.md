# [S] Steer only QUIC by connection ID

## Goal

The reuseport steering filter steers QUIC by connection ID and leaves every
other datagram to the kernel's 4-tuple hash, so an RTP, SRT, or STUN flow
lands on one shard for its whole life and the demux on that shard sees all
of it. Both reuseport groups, moq-tokio's workers and moq-uring's, get this
from the one filter they share.

## Plan

The filter (`moq-sock`'s `shard` module today, `moq-quic-udp` once
[quic/shard](/quest/m1/quic/shard.md) moves it) reads byte 1 or byte 6 by the
long-header bit and reduces it modulo the group size. A non-QUIC datagram
goes through the same arithmetic: RTP and RTCP are steered by timestamp and
SSRC bytes and scatter across shards, SRT data is steered by a sequence
byte, and STUN and DTLS, whose byte 1 or 6 barely varies, pile onto one shard.

Decided 2026-10-05: its own quest, required by the
[UDP demux](/quest/m2/one-port/udp-demux.md) because the demux's flow table is
per shard. Folding it into [the io_uring demux](/quest/m2/uring-demux.md) was
rejected: moq-tokio's group has the same exposure.

The filter tests the first byte against the QUIC ranges in the
[questline README](/quest/m2/one-port/README.md) and returns an out-of-range
index for anything else, which the kernel answers with its 4-tuple hash.

Open: an SRT data packet whose sequence number starts in 0x40 to 0x7F looks
like a QUIC short header, and the byte alone cannot tell them apart. That is
half of the initial sequence numbers an encoder may pick, so it needs flow
state, not a better byte test. Options include a `SK_REUSEPORT` eBPF program
with a 4-tuple map the demux fills when it pins a flow, or a shard handing a
pinned flow's datagrams to its owner. Pick by measured cost; if neither fits
an [S], split it out rather than shipping SRT that breaks on half its flows.

Tests: per-flow shard stability for an RTP flow, an SRT flow (including a
sequence number in the QUIC-shaped range), and a STUN flow, each across many
packets, through both moq-tokio's group and the `moq-sock` group directly.
QUIC steering by connection ID is unchanged.

## Related

- [Shard the QUIC endpoint](/quest/m1/quic/shard.md) - moves the filter this edits into `moq-quic-udp`
