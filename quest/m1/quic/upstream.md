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

Offer the seven BBR correctness fixes (moq-noq 1.3.1, #4206) and
their [loss](/quest/m2/quic-bbr-loss-parity.md) and
[starvation](/quest/m1/quic/bbr-app-limited-edges.md) follow-ups with their
regressions before promoting BBR as the default. Reuse existing
upstream work, particularly [PR #802](https://github.com/n0-computer/noq/pull/802),
and preserve attribution; the fork found that #802's `has_sample()` check
fires one ACK late under noq's callback order and that its 1-ms floor keeps
sub-millisecond paths at the placeholder rate, so offer both back there. The
additive `PacketId` callbacks answer upstream's TODO on the `Controller`
impl. Fixes can be offered as their shapes settle; upstream acceptance never
gates the fork's corrected release.

Then the feature proposal order, each linked to its producing quest:

0. BBR3 as `TransportConfig`'s default controller (noq-proto 1.2.0 defaults
   to Cubic, `config/transport.rs:586`, while MoQ has run BBR3 by default
   since it adopted noq): a one-line change whose point is that every noq
   and iroh user, the maintainers included, runs the controller MoQ depends
   on, so its regressions are found upstream and not only here;
1. per-stream acknowledgment progress ([ACK progress](/quest/m2/quic-ack-progress.md));
2. `RESET_STREAM_AT` ([reliable reset](/quest/m1/quic/reliable-reset.md));
3. hierarchical send groups ([scheduler](/quest/m1/quic/scheduler.md));
4. per-stream deadlines ([deadlines](/quest/m2/quic-deadline.md));
5. the qmux crate over the shared stream state machine ([qmux](/quest/m2/quic-qmux.md)).

The m2 features (keep-alive by deadline, careful resume as a `Controller`
wrapper, ECT(1) marking, the media-headroom mechanism) are offered when they
land, but do not gate this quest: an m1 quest must not wait on m2 work. The
next experiments (receive timestamps, GCC, FEC, kernel pacing, send
batching, buffer pools, the natural-drain check) join the list only with a
positive verdict.

Record in this quest what upstream accepted, what it asked to see as an
extension crate, and what it declined; a declined change stays in the fork
with the link beside it. The quest completes when the list above has been
offered and answered.

## Required

- [Mark BBR starvation wherever the source runs dry](/quest/m1/quic/bbr-app-limited-edges.md)

- [Reliable stream reset](/quest/m1/quic/reliable-reset.md)
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md)

## Related

- [Keep-alive by deadline](/quest/m2/quic-keep-alive.md),
  [Careful resume on reconnect](/quest/m2/quic-careful-resume.md),
  [L4S on the backbone](/quest/m2/quic-ecn.md),
  [Discover media headroom](/quest/m2/quic-probe.md) - m2 features offered
  upstream when they land
