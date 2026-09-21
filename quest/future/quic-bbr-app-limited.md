# [M] Measure natural draining before BBR ProbeRTT

## Goal

A measured adopt-or-retain decision on allowing natural media drains to
satisfy ProbeRTT. Reduce demonstrated frame deadline interference without
stale minimum RTT, persistent queues, or unfairness. Application limitation
alone is not evidence that the path drained.

## Plan

Use the corrected released fork as baseline. ProbeRTT targets half the
estimated BDP, floored at the minimum pipe window; entering the mode need
not withhold useful bytes. First locate actual cwnd stalls and deadline
misses using steady low-rate traffic, 30-fps frames with keyframes, and gaps
shorter and longer than RTT. Include both learned capacity above the media
rate and a sender that has never measured more than its source rate.

Compare the baseline with Google's explicit idle accounting and bounded
natural-drain credit. [QUICHE BBR3](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr3_sender.cc#L534)
postpones RTT aging across zero-inflight idle intervals; its
[tests](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr3_simulator_test.cc#L1724)
cover restart behavior. These source defaults are not deployment evidence.
Noq's Linux-style idle-restart suppression is narrower.

A candidate credits sustained low inflight, a completed sampled round, and
fresh RTT evidence toward ProbeRTT. Determine the predicate experimentally;
neither the app-limited label nor a stale BDP threshold is sufficient. Bound
deferral and retain fallback after stale evidence, increased base RTT, or a
path change. Blanket skipping is only a diagnostic control. Retain the
simpler baseline if no material interference is demonstrated.

Use identical media traces and seeds. Start with short and long RTT paths,
then test capacity changes, base-RTT changes, competing bulk traffic, shallow
queues, loss bursts, and compressed or delayed ACKs. Record deadlines and
frame completion, withheld useful bytes, sender backlog separately from
network queueing, RTT-baseline error, sample labels, and mode residence.
Set latency and fairness acceptance limits before tuning. Persist the
harness in CI, with broader network cases at least nightly, and a verdict
with pinned sources/configurations. Any adopted production policy gets a
separate implementation quest; this study does not silently change defaults.

## Required

- [Release BBR fixes](/quest/next/quic/bbr-release.md) - measure the corrected controller through MoQ's dependency chain

## Related

- [Discover media headroom](/quest/next/quic/probe.md) - preserving an estimate and discovering spare capacity are separate problems
- [Google BBR comparison](/quest/future/quic-bbr-google.md) - separate growth and precautionary-probing decisions
