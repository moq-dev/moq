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

Decided 2026-11-01:

- ✅ The transport fills a probe cluster at the controller's own pace rather
  than capping the duplicates at the target, so GoogCC's probe reads the path
  (`Controller::paces_probe`). GoogCC's clusters bound the overhead.
- ✅ The shared comparison's sender model is an encoder retuned to
  `bandwidth_estimate`, as moq-cli and moq-ffi do, rather than bulk alone.

What remains: `tests::probe::rises_toward_target_under_gcc` now reaches
5.0 Mbit/s against the 4 Mbit/s target on all 8 seeds, but 7.9s after the
path opens, against BBR3's 5s budget, and the baseline without a target
reaches 4.9 Mbit/s at the same moment. GoogCC probes in the
application-limited region only five seconds after it last probed or
entered that region, and caps every probe at its default 5 Mbit/s maximum,
so with an 8 Mbit/s target the estimate stops at 5.0 Mbit/s. Whether to hand
GoogCC the session's probe target (its `OnMaxTotalAllocatedBitrate` probes
when the allocation rises and caps probes at twice it), raise its maximum,
or accept GoogCC's cadence is undecided. Once that lands,
`rises_toward_target_under_gcc` asserts the rise as `rises_to_target` does
for BBR3, if the rise budget applies to GCC.

## Related

- [Realtime uplink congestion control](/quest/m2/quic-realtime/README.md) - the bake-off GCC is a candidate in
