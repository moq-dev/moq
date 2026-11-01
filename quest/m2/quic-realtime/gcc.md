# [S] GCC candidate

## Goal

Under `moq_congestion::Gcc`, moq-quic's native probing raises the estimate to
the session's probe target once the path allows it, so a rung stopped for want
of an estimate comes back, as it does under BBR3.

## Plan

GoogCC is vendored in `rs/moq-congestion/src/gcc/`, refreshed to WebRTC
556f7eec27, trimmed to what QUIC has, and driven as a `Controller` from
receive timestamps. Its probe clusters report `probe_inflight`, and its
queuing delay, throughput, and reaction time on the sim replicas of the
cellular presets are in the PR that landed it.

What remains: `tests::probe::rises_toward_target_under_gcc` ends at
3.2 Mbit/s against a 4 Mbit/s target on every seed, while GCC without the
target reaches 4.9 Mbit/s. GoogCC probes at twice its estimate and reads a
probe at the slower of its send and receive rates, while the transport sends
the duplicates filling a probe at no more than the target in all, so past a
frame's media they trail the cluster and the reading falls short. Which side
changes is undecided: the controller learning the target (GoogCC's
`OnMaxTotalAllocatedBitrate` takes it, and caps its probes at twice it), or
the transport letting a probe's duplicates keep the controller's probe rate.
Once one lands, `rises_toward_target_under_gcc` asserts the rise as
`rises_to_target` does for BBR3.

## Related

- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - the bake-off GCC is a candidate in
