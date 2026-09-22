# [M] L4S on the backbone

## Goal

Relay-to-relay sessions can mark ECT(1) with a scalable response, measured
behind an option that is off by default, and a deployment whose network
mangles marks can turn ECN off explicitly. Paths that strip or mangle the
marks fall back to no ECN, and a viewer's session is unaffected.

## Plan

Classic ECN is already end to end on both runtimes; this quest is the
fork-side half. noq-proto has no ECN knob: `sending_ecn` is hardcoded on
per path and only an ACK without counts turns it off, so both `off` and
`ect1` need the fork.

- In the fork: an `Ect1` marking option and the accounting to keep the two
  codepoints apart, so an L4S response (proportional to the CE fraction per
  RTT, per RFC 9330 to 9332) can be tried as a `Controller` change without a
  transport change. Off by default.
- `moq-tokio`'s `[quic]` section gains `ecn = off | ect0 | ect1`, ect0 by
  default (today's behavior on tokio), and `doc/bin/relay/config.md`
  documents it in the same PR.
- Measure on the netem bottleneck from the
  [ECN study](/quest/next/quic/ecn-measure.md) with `dualpi2` marking against
  the same bottleneck dropping: queueing delay, goodput, loss. The study's
  provider verdict decides whether the result matters outside the lab;
  if neither Linode nor OVH preserves the marks, L4S stays off and the
  result is written down here.
- ECN visibility stays on the wire: the study observes marks with tcpdump,
  and exposing per-path ECN state in stats is a later quest if operators
  need it.

## Required

- [Measure ECN on the backbone](/quest/next/quic/ecn-measure.md) - the
  provider verdict this quest acts on
