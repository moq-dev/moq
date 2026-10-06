# [S] Steer only QUIC by connection ID

## Goal

The reuseport steering filter steers a datagram by connection ID only when
it is recognizably QUIC, and leaves every other datagram to the kernel's
4-tuple hash, so an RTP, SRT, or STUN flow
lands on one shard for its whole life and the demux on that shard sees all
of it. Both reuseport groups, moq-tokio's workers and moq-uring's, get this
from the one filter they share, and both backends advertise QUIC-bit greasing
off so their peers' short headers stay recognizable.

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

Decided 2026-10-05: classify on more than the first byte and stay [S].
Rejected: re-sizing the quest to [M] for flow state, and a userspace hand-off
of datagrams between shards. A first byte cannot separate an SRT data packet
whose sequence number starts in 0x40 to 0x7F from a QUIC short header, and
that is half the initial sequence numbers an encoder may pick, but a few more
bytes can:

- Long header (0x80 set): steer as QUIC only when bytes 1 to 4 are a
  supported QUIC version (v1 `0x00000001`, v2 `0x6b3343cf`). RTCP never
  matches and RTP almost never does (about 2^-24 per packet for PT 0 or
  0x6b), which RTP tolerates. Do not also require the fixed bit: RFC 9287
  lets a client grease its Initials.
- Short header (0x40 set, 0x80 clear): steer as QUIC only when the
  destination connection ID carries a fixed relay magic of N bytes beside the
  existing shard byte. This is a CID-format change: every CID the relay
  issues, the initial source CID and every NEW_CONNECTION_ID, embeds the
  magic. Its owner is the shard CID generator (moq-tokio's and moq-uring's
  today, `moq-quic`'s once [quic/shard](/quest/m1/quic/shard.md) replaces
  them). Say whether the CID grows past today's 8 bytes or the magic takes
  random bytes from it.
- Everything else returns an out-of-range index, which the kernel answers
  with its 4-tuple hash.

The magic must be a pattern no valid SRT data header can carry, not merely
an unlikely one. SRT's sequence, message, and flag fields are structured,
and a retransmission repeats them with only the R flag changed, so a header
that matches once could match on every retry, and ARQ would not recover it.
One candidate: the DCID byte that overlays SRT's flag byte with KK = 0b11,
which the SRT draft reserves for control packets (verify this against the
draft).

Greasing off belongs here, on both backends (moq-tokio's and moq-uring's
QUIC configs), because this filter is the first thing that reads the fixed
bit: a peer that may grease clears it on about half its short headers, and
they would fall to the 4-tuple hash and miss their CID's shard. Moved here
2026-10-05 from udp-demux and uring-demux, which land after this.

Fallback: if no magic is provably excluded from SRT data, or the CID length
or a future QUIC-LB format cannot fit one, split out `one-port/shard-flows.md` [M], an `SK_REUSEPORT` eBPF
program with a 4-tuple map the demux fills when it pins an SRT flow,
required by [SRT on the shared socket](/quest/m2/one-port/srt-demux.md).

Tests: an RTP flow, an SRT data flow whose first byte is in 0x40 to 0x7F, a
STUN flow, and a DTLS flow each stay on one shard across many packets,
through both moq-tokio's group and the `moq-sock` group directly. The SRT
flow includes retransmissions (R set) of the same packets. QUIC v1 and v2
long headers and short headers still reach their CID's shard. A client
talking to each backend receives transport parameters without
`grease_quic_bit`, and every short header it sends reaches its CID's shard.

## Related

- [Shard the QUIC endpoint](/quest/m1/quic/shard.md) - moves the filter this edits into `moq-quic-udp`
