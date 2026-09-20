# [S] Measure ECN on the backbone

## Goal

A written verdict on classic ECN between relays: whether a marking
bottleneck reduces the rate before its queue overflows instead of after,
with the numbers, and whether Linode's and OVH's networks preserve ECT(0)
and CE between relays. No code lands; the verdict is recorded in
[L4S on the backbone](/quest/m2/quic/ecn.md), which acts on it.

## Plan

A manual procedure on Linux, run as root, with the commands and what to
record written here so the dualpi2 run and any later provider re-check
repeat it.

- Bottleneck: two relays on the io_uring runtime across a netem link with a
  rate limit and delay, once with `fq_codel` marking (`ecn` on) and once
  with the same qdisc dropping (`noecn`). Measure queueing delay at the
  bottleneck, goodput, and loss under a moq-bench media profile. The
  question is whether the marking run holds a shorter queue at the same
  goodput.
- Providers: between a Linode relay and an OVH relay, `tcpdump -v` on each
  end shows the TOS byte of arriving packets. Record whether ECT(0) arrives
  intact in both directions and whether any CE appears; ECN is only alive
  when both directions preserve the marks, since a stripped return path
  means ACKs without counts. Repeat the capture for cross-provider and
  same-provider pairs.
- Record the qdisc parameters, the moq-bench profile, the kernel version,
  and the tcpdump summaries beside the numbers in the L4S quest's Plan.
  If neither provider preserves the marks, say so there: L4S stays off and
  the marking response is only a lab result.

## Required

- [ECN on the io_uring UDP path](/quest/m2/quic/ecn-uring.md) - the relays
  under test must mark
