# [M] Make BBR ACK cleanup scale with completed packets

## Goal

Large BBR flights do not make every ACK scan all outstanding packets.
Packet bookkeeping scales with acknowledged, lost, and expired entries while
preserving bandwidth samples and congestion behavior. Ship the improvement
through MoQ's published dependency chain without changing public APIs or wire
behavior.

## Plan

The fix lives in moq-dev/noq. The baseline is the current releases: the
workspace pins moq-noq 1.3.2 and 2.0.0 is out. In both, `on_end_acks`
(`noq-proto/src/congestion/bbr3/mod.rs:1845`) runs `retain` over all tracked
packets, then scans them again to mark stale entries. Draining a flight with fixed-size ACK batches has quadratic total
cleanup work. [Google QUICHE](https://github.com/google/quiche/blob/c961965aa3ee8f2b6f05ebcac794f7854101adcd/quiche/quic/core/congestion_control/bandwidth_sampler.cc#L377)
uses packet-number lookup and obsolete-prefix reclamation; use that as a
reference without copying a TCP or single-space assumption into QUIC.

Retain an optimized benchmark sweeping 1,000, 4,000, and 16,000 outstanding
1200-byte packets against ACK batches of 1, 8, and 32. Time the controller
callbacks, excluding fixture setup and the simulator's own queue searches.
The initial local median-of-three measurements at eight packets per ACK were
0.650, 11.310, and 162.873 ms respectively. These are workload evidence, not
portable timing thresholds or a network-throughput comparison with Google.
Add steady-flight cases so draining the table cannot hide recurring costs.

Replace whole-flight cleanup with indexed packet state and incremental
reclamation. Let the implementation choose the simplest representation that
handles sparse packet numbers, reordering, and separate Initial, Handshake,
and Data spaces. Bound retained state without repeatedly sweeping live
entries or scanning large unused packet-number gaps. Keep the existing
sampling and congestion policies unchanged; loss-sample repair belongs to
its own quest.

Extend existing tests for ordered, reordered, duplicate, and batched ACKs;
loss and spurious loss; expiry; overlapping packet numbers in different
spaces; and handshake-space discard. Compare samples and control outputs
against the unchanged baseline on valid callback traces, allowing only the
bookkeeping change. A late event for expired state must remain safe and must
not reuse another packet's metadata.

Acceptance is the removal of the whole-flight factor from per-ACK work,
with measured scaling and bounded retained memory under reordering and loss.
Report CPU time and memory across both benchmark axes, including small
flights, and explain any remaining logarithmic or amortized cost. Keep the
regressions in fork CI and the benchmark matrix at least nightly. Do not
substitute one hardware-specific millisecond limit for the scaling check.

Land the fix in the fork, offer it upstream or record why not, publish an
immutable fork release, and pin the corrected dependency chain here before
completing this quest. Do not wait for the broader QUIC stack release. Update internal packet-lifetime comments inline; no new user guide
is needed.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - lands in `moq-quic`, not the frozen fork

## Related

- [BBR starvation edges](/quest/m1/quic/bbr-app-limited-edges.md) - also edits `bbr3/mod.rs`; one owner there at a time
- [Benchmark comparisons](/quest/m1/performance-comparisons.md) - reusable measurement guidance, not a prerequisite for this fix
- [Loss sampling](/quest/m2/quic-bbr-loss-parity.md) - preserve packet metadata needed by the separate loss-sample repair; both edit `bbr3/mod.rs`, so sequence them
