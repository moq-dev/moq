# [S] BBR3 under an application-limited media flow

## Goal

Evidence that noq's BBR3 behaves for a sender that never fills its window: a
live publisher or a relay egress at the encoder's rate. The six confirmed
correctness defects are fixed and released first. This study measures
remaining behavior of the corrected controller rather than rediscovering
those defects.

## Plan

ProbeRTT targets half the estimated BDP, floored at the minimum pipe window;
it does not simply halve the current congestion window. Its 200-ms hold and
5-second minimum interval do not imply an unconditional pause every 5 seconds.
Check a media-shaped flow at half the bottleneck rate with the impaired path
profile:

- whether `ProbeRTT` constrains an app-limited flow, comparing inflight with
  the actual BDP-based limit and minimum window, and whether it is skipped or shortened
  when the flow is app-limited, as BBRv3 permits when the minimum RTT was
  refreshed recently;
- whether `bandwidth_estimate` stays at the last validated capacity or decays
  toward the encoder rate, which is what feeds MoQ's PROBE and the encoder
  ladder;
- whether restart-from-idle after a keyframe gap ramps within one RTT.

Report rate, estimate, and latency traces per phase. Fix what is wrong in
the fork with a regression test; record what is fine.

## Required

- [Release BBR fixes](/quest/next/quic/bbr-release.md) - use the corrected controller through the actual MoQ dependency chain

## Related

- [Google BBR comparison](/quest/future/quic-bbr-google.md) - separate verdict on the two algorithm differences

- [Probe by early retransmission](/quest/next/quic/probe.md) - the estimate
  above the encoder rate that an app-limited sender cannot otherwise get
