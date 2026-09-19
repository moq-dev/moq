# [M] ECN on the backbone

## Goal

Relay-to-relay sessions validate ECN, send ECT marks, and feed CE counts
from the peer's ACK frames to the congestion controller, so a marking
network reduces the rate before a queue overflows instead of after. Paths
that strip or mangle the marks fall back to no ECN, validated per RFC 9000
section 13.4, and a viewer's session is unaffected.

## Plan

noq-proto already parses and reports ECN counts in ACK frames and BBR3 has
the `on_congestion_event` hook; what is missing is validation, the sending
side, and a policy for which marks to send.

- In the fork: implement ECN validation on path establishment, ECT(0) by
  default with ECT(1) as the L4S option, and treat a CE increase as a
  congestion signal in Cubic and as BBR3 specifies.
- `moq-tokio`'s `[quic]` section gains `ecn = off | ect0 | ect1`, off for
  clients, on for the relay's cluster listener. The io_uring UDP path
  sets and reads the TOS byte through the cmsg it already builds for GSO.
- Measure on a netem bottleneck with an ECN-marking qdisc (`fq_codel` or
  `dualpi2`) against the same bottleneck dropping: queueing delay, goodput,
  and loss. Record whether Linode's and OVH's networks preserve the marks
  end to end; if neither does, ship the code off by default and say so.

## Required

- [Fork noq](/quest/m2/quic/fork.md) - the validation and marking live there
