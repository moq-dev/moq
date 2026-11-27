# [L] A bonded connection sends on every available path at once

## Goal

With two or more available paths, a moq-quic connection sends each packet
once, on the lowest-RTT path that has congestion window room, so its rate
approaches the sum of the paths' capacities. Data lost on one path is
retransmitted on whichever path the scheduler picks next, so a dying link
costs one loss-detection delay and no extra bytes. Real-time media arrives
no later than it would on the best single path. Backup paths stay idle
while an available one lives.

## Plan

Decided in the [epic](/quest/m3/quic-multipath/README.md): striping first,
with the scheduler designed as configurable and
[redundancy](/quest/m4/quic-multipath-redundancy.md) following. Recommended:
the scheduler is an internal seam with this one policy, and no public knob
appears until the second policy needs one.

What needs judgment, to settle by measurement rather than up front:

- Unequal RTTs. A stream split across a 20 ms and an 80 ms path waits for
  the slow path's bytes. Lowest-RTT-first spills to the slow path only when
  the fast one is window-limited; measure whether that is enough, and
  whether the highest-priority send group should stay on the fastest path.
- Acknowledgments go on the fastest live path, so one slow path does not
  delay the others' loss detection.
- Pacing and bandwidth probing (`bandwidth_probe.rs`) run per path.
- The receiver's reassembly is bounded as today; reordering across paths
  must not grow what a peer can make it hold.

Measure on the seeded sim with bufferbloat-shaped links and the media
workloads in `tests/sim/workloads.rs`, over equal links (5 + 5 Mb/s) and
unequal ones (20 + 2 Mb/s, 20 ms + 80 ms): goodput against the sum, group
delivery latency at the median and 99th percentile against the best single
path, and the gap after a silent cut. Then repeat the throughput and cut
cases on built binaries with the namespace harness from
[paths on Linux](/quest/m3/quic-multipath/paths-linux.md).

Public API: none. Behavior: a connection with several available paths uses
them all. Wire: none.

## Required

- [moq-tokio opens extra paths on Linux, and relays accept them](/quest/m3/quic-multipath/paths-linux.md) - built binaries and the namespace harness to prove it on
