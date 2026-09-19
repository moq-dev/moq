# [S] BBR3 under an application-limited media flow

## Goal

Evidence that noq's BBR3 behaves for a sender that never fills its window: a
live publisher or a relay egress at the encoder's rate. The concern is
`ProbeRTT`, which halves the congestion window for 200 ms every 5 s, and the
bandwidth model decaying while samples are app-limited. Either the flow
keeps its rate and estimate across those phases, or the fork changes what
BBR3 does when app-limited and the change is measured.

## Plan

BBR3 in noq marks app-limited rounds, and its `ProbeRTT` uses a 0.5 cwnd
gain. Check, on a media-shaped flow at half the bottleneck rate with the
impaired path profile:

- whether `ProbeRTT` ever constrains an app-limited flow (in-flight should
  already be below half the window) and whether it is skipped or shortened
  when the flow is app-limited, as BBRv3 permits when the minimum RTT was
  refreshed recently;
- whether `bandwidth_estimate` stays at the last validated capacity or decays
  toward the encoder rate, which is what feeds MoQ's PROBE and the encoder
  ladder;
- whether restart-from-idle after a keyframe gap ramps within one RTT.

Report rate, estimate, and latency traces per phase. Fix what is wrong in
the fork with a regression test; record what is fine.

## Required

- [Fork noq](/quest/m2/quic/fork.md) - any fix lives there

## Related

- [Probe by early retransmission](/quest/m2/quic/probe.md) - the estimate
  above the encoder rate that an app-limited sender cannot otherwise get
