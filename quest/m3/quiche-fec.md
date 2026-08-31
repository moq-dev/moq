# [L] Quiche FEC experiment

## Goal

A measured verdict on transport-level forward error correction: does spending
redundancy bytes ahead of loss beat retransmission on delivery latency, and
at what loss rate does it start paying? Scope: fork-controlled peers only -
cluster links and native clients on the moq-dev/quiche fork, where all fork
work lands - because FEC needs a receiver that reconstructs, and browsers or
vanilla WebTransport peers cannot consume custom frames. Ship nothing by
default; findings are recorded in this Plan, and abandonment is a valid
outcome.

## Plan

- Measure before building: sample the actual loss distribution on real
  subscriber connections first (the moq.pro fleet is the available downstream
  source). If real losses are rare and bursty rather than steady, FEC protects
  against a regime the deployment does not have, and the experiment ends
  there.
- Candidate shapes, cheapest first: parity across a group's tail packets
  (bounded by the group's lifetime, aligning with `max_age` eviction),
  datagram-level parity, and stream-data FEC frames in the style of
  draft-michel-quic-fec. All live in the fork; none change the MoQ wire.
- The redundancy budget is shared with
  [probe](/quest/m2/quiche/probe.md): early retransmission is already a
  repetition code, so the experiment must compare FEC against it, not only
  against plain ARQ - the probing quest may have already banked most of the
  win.
- Rig and limits mirror the [GCC experiment](/quest/m3/quiche-gcc.md): netem
  loss shapes, moq-bench media profiles, honesty section beside every number.
