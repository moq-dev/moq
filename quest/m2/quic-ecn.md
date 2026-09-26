# [M] L4S on the backbone

## Goal

Relay-to-relay sessions can mark ECT(1) with a scalable response, measured
behind an option that is off by default, and a deployment whose network
mangles marks can turn ECN off explicitly. Paths that strip or mangle the
marks fall back to no ECN, and a viewer's session is unaffected.

## Plan

ECT(0) marking and ACK ECN counts are carried end to end, but BBR's
[classic CE response](/quest/m1/bbr-classic-ecn.md) must be corrected before
it is used as the baseline. This quest adds the scalable policy separately.
noq-proto has no ECN knob: `sending_ecn` starts on per path and validation
failure or an ACK without counts turns it off, so both `off` and `ect1`
need the fork.

- In the fork: an `Ect1` marking option and the accounting to keep the two
  codepoints apart, so an L4S response (proportional to the CE fraction per
  RTT, per RFC 9330 to 9332) can be tried as a `Controller` change without a
  transport change. Off by default.
- `moq-tokio`'s `[quic]` section gains `ecn = off | ect0 | ect1`, ect0 by
  default (today's behavior on tokio), and `doc/bin/relay/config.md`
  documents it in the same PR.
- Measure on the netem bottleneck from the
  [ECN study](/quest/m1/quic/ecn-measure.md) with `dualpi2` marking against
  the same bottleneck dropping: queueing delay, goodput, loss. The study's
  provider verdict decides whether the result matters outside the lab;
  if neither Linode nor OVH preserves the marks, L4S stays off and the
  result is written down here.
- ECN visibility stays on the wire: the study observes marks with tcpdump,
  and exposing per-path ECN state in stats is a later quest if operators
  need it.

## Required

- [Classic BBR ECN](/quest/m1/bbr-classic-ecn.md) - establish a corrected released classic response before comparing L4S
- [Measure ECN on the backbone](/quest/m1/quic/ecn-measure.md) - the
  provider verdict this quest acts on
