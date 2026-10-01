# [L] QUIC GCC egress experiment

## Goal

Record a measured verdict on WebRTC-style delay-based congestion control for
subscriber-facing media egress against noq's production
controller. Ship it only if it reduces queueing delay and rate variation
without collapsing throughput. A written abandonment is a successful outcome.

## Plan

Implement the candidate in the fork as a `congestion::Controller`, driven by
the per-packet receive timestamps the receive-timestamps spike delivers; the
sender-side inter-arrival filter is what makes it GCC rather than another
RTT-based controller. If it ships, it joins MoQ's backend-neutral congestion
family as `CongestionControl::RealTime`, beside `Loss` (Cubic) and `Delay`
(BBR3); MoQ owns which algorithm each name means, and the config never
exposes algorithm names. Egress only: relay ingest keeps BBR3.

Use the moq-bench media profiles under reproducible netem delay, loss, and
bottleneck rates. Decide on p95 queueing delay, delivered-rate variation,
goodput, and starvation against a competing low-rate interactive MoQ stream.
Anything measured on real relay hardware needs a dedicated real-NIC rig rather
than loopback.

State the experiment's boundary beside the result: netem cannot establish
behavior against production cross traffic or real wifi and cellular loss.

Receive timestamps are native-only: browsers never negotiate the extension,
so GCC can only target native peers or relay-to-relay sessions, not browser
egress. Decided in the 2026-09-30 audit: parked in m3 until such a consumer
exists.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the change lands in `moq-quic`, not the frozen fork
- [Receive timestamps](/quest/m3/quic-receive-ts.md) - the per-packet
  arrival times the delay filter runs on

## Related

- [noq#818](https://github.com/n0-computer/noq/issues/818) - the GCC proposal to n0; matheus23 asked for a non-breaking `Controller` trait
