# [M] A collapsing uplink lowers the bandwidth estimate promptly

## Goal

When a path's capacity collapses, the bandwidth estimate a sender reports
(the grant simulcast rungs follow) falls as soon as BBR3's own loss response
cuts, instead of holding the old rate for seconds. A before-and-after
measurement with the sparse-sender shaper harness shows the drop.

## Plan

Decided 2026-11-05 (maintainer, follow-up from
[moq-dev/moq#7671](https://github.com/moq-dev/moq/pull/7671)): ✅ the
reported estimate also honors BBR3's loss-driven bound; m1, beside the native
rung-disable proof. Not chosen: m2, or an investigation-only quest.

[moq-dev/moq#7952](https://github.com/moq-dev/moq/pull/7952) measured it and
made two changes. The estimate now reports BBR.bw, so it includes
`bw_shortterm`. BBR3 now exits recovery after an ACK's model steps, so
Startup's high-loss exit can fire. Before that, an application-limited sender
never left Startup: RFC 9002 recovery ends on the same ACK that ends the loss
round. These changes leave two holds in place:

- On a lossy collapse (sim, 2 Mbit/s to 100 kbit/s through a one-second
  drop-tail queue), BBR3 now cuts `bw_shortterm` about 4 s after the collapse.
  Leaving ProbeRTT or entering REFILL then lifts the cut, as the draft says,
  and `max_bw` holds the old rate for two probe cycles: about 17 s on plain
  BBR3. With the delay bound on, the hold is 8.3 s instead of 12.5–14.9 s.
- The `rung-disable` FFI run's 500 kbit/s step drops nothing, because the
  shaper queues instead. Its 5.6 s disable is Startup's full-bandwidth exit
  plus two `max_bw` filter advances. No loss bound takes part in it, so the
  changes leave it unchanged.

## Waiting

- Maintainer: decide what still counts as prompt. Options:
  1. Close this quest with #7952. BBR3's own response time is then the bound,
     and the bufferbloat hold goes to the realtime controller choice in
     [quic-realtime](/quest/m2/quic-realtime/README.md), where the delay bound
     already brakes on queue growth.
  2. Keep it open for an estimator that follows a cwnd-limited sender's
     delivery rate before `max_bw` ages out.
  3. Make `CongestionControl::Delay` turn on the delay bound.

  Recommended: 1.
