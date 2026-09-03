# [L] QUIC FEC experiment

## Goal

Record a measured verdict on transport-level forward error correction: whether
spending redundancy before loss beats retransmission on delivery latency, and
at what loss regime it pays. Scope is fork-controlled native peers because
browsers and vanilla WebTransport peers cannot consume custom frames. Ship
nothing by default; abandonment is a valid result.

## Plan

Measure the production loss distribution before building. If losses are too
rare or bursty for the candidate codes to recover inside a group's lifetime,
record that result and stop.

Try the cheapest bounded shapes first: parity across a group's tail packets,
datagram-level parity, then stream-data FEC frames. Keep all framing in the
selected QUIC fork; the MoQ wire does not change.

Compare against [early retransmission](/quest/m2/quic/probe.md), not only plain
ARQ. Both spend the same spare-bandwidth budget on redundancy. Use the same
netem and real-NIC limits as the
[GCC experiment](/quest/m3/quic-gcc.md), and report delivery latency, goodput,
redundancy cost, and unrecovered group loss.

## Required

- [Probe capacity by early retransmission](/quest/m2/quic/probe.md) - supplies
  the baseline this experiment must beat
