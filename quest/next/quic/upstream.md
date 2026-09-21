# [M] Upstream the fork

## Goal

Every general change the fork carries has been offered to n0-computer/noq as
a reviewable pull request, and the fork's carried set is only what upstream
declined or what is MoQ-specific. The noq maintainers have seen this roadmap
before the first proposal lands, so nothing arrives as a surprise.

## Plan

Fork now, upstream opportunistically: this quest is the "opportunistically"
half, and it completes the line rather than gating it. Each feature quest
lands in the fork on MoQ's schedule; once a feature has shipped in a MoQ
release and its shape has stopped moving, split it into an upstream PR with
the tests it landed with.

Proposal order, most general first, each linked to the quest that produced
it:

0. BBR3 as `TransportConfig`'s default controller (noq-proto 1.2.0 defaults
   to Cubic, `config/transport.rs:586`, while MoQ has run BBR3 by default
   since it adopted noq): a one-line change whose point is that every noq
   and iroh user, the maintainers included, runs the controller MoQ depends
   on, so its regressions are found upstream and not only here;
1. per-stream acknowledgment progress ([ACK progress](/quest/next/quic/ack-progress.md));
2. `RESET_STREAM_AT` ([reliable reset](/quest/next/quic/reliable-reset.md));
3. keep-alive by deadline ([keep-alive](/quest/next/quic/keep-alive.md));
4. hierarchical send groups ([scheduler](/quest/next/quic/scheduler.md));
5. careful resume as a `Controller` wrapper ([careful resume](/quest/next/quic/careful-resume.md));
6. ECT(1) marking and its accounting ([L4S](/quest/next/quic/ecn.md));
7. per-stream deadlines ([deadlines](/quest/next/quic/deadline.md));
8. capacity probing by early retransmission ([probe](/quest/next/quic/probe.md));
9. the qmux crate over the shared stream state machine ([qmux](/quest/next/quic/qmux.md)).

The next experiments (receive timestamps, GCC, FEC, kernel pacing, send
batching, buffer pools, the BBR3 app-limited check) join the list only with
a positive verdict.

Record in this quest what upstream accepted, what it asked to see as an
extension crate, and what it declined; a declined change stays in the fork
with the link beside it. The quest completes when the list above has been
offered and answered.

## Required

- [Fork noq](/quest/next/quic/fork.md) - the fork the proposals split from
- [Per-stream ACK progress](/quest/next/quic/ack-progress.md)
- [Reliable stream reset](/quest/next/quic/reliable-reset.md)
- [Keep-alive by deadline](/quest/next/quic/keep-alive.md)
- [Hierarchical stream scheduling](/quest/next/quic/scheduler.md)
- [Careful resume on reconnect](/quest/next/quic/careful-resume.md)
- [L4S on the backbone](/quest/next/quic/ecn.md)
- [Per-stream deadlines](/quest/next/quic/deadline.md)
- [Probe by early retransmission](/quest/next/quic/probe.md)
- [qmux on the QUIC stream state machine](/quest/next/quic/qmux.md)

## Related

- [Receive timestamps](/quest/future/quic-receive-ts.md), [GCC](/quest/future/quic-gcc.md),
  [FEC](/quest/future/quic-fec.md), [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) -
  experiments that join the list with a positive verdict
