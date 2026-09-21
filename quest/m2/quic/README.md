# Own the QUIC stack

## Goal

MoQ owns the QUIC features it needs. noq, the Quinn-derived stack n0
maintains for Iroh, is the parent; the moq-dev fork carries what MoQ needs on
MoQ's schedule and offers it upstream when it is general. One core serves the
tokio backend, the thread-per-core `moq-uring` backend, iroh, and qmux. The
features are per-stream acknowledgment progress, reliable stream resets,
hierarchical stream scheduling with per-broadcast fairness, the shared stream
state machine used by qmux, capacity probing by retransmission, per-stream
deadlines, deadline-based keep-alive, wider limits for relay peers, careful
resume, and ECN. The experiments that may join them (GCC, FEC, receive
timestamps, kernel pacing, buffer pools) live in m3.

## Plan

Everything here assumes the single noq stack. The [fork](/quest/m2/quic/fork.md)
is the first quest in the line and most others require it.

Rules the line keeps:

- a carried change lists its upstream PR or the reason it has none;
- the fork's packages are `moq-noq-proto`, `moq-noq`, and `moq-noq-udp`,
  never a crate that impersonates the parent;
- published MoQ crates depend on crates.io releases of the fork, never a
  workspace-only Cargo patch or a mutable branch;
- MoQ's config names congestion families (`Loss`, `Delay`, and `RealTime`
  once GCC ships), never algorithms; noq's public `Controller` trait is the
  seam experiments plug into, and MoQ owns which algorithm each family means.

The scheduling contract has three levels: strict subscription priority,
byte-fair service between send groups at the same priority, then the
subscription's chosen group order within its own bucket. On a relay-to-relay
session with fairness enabled, the send group is the broadcast. The default
MoQ order is newest group first; an ordered subscription keeps oldest first.
This is a transport API change, not a MoQ wire change.

## Quests

- [Fork noq](/quest/m2/quic/fork.md) - moq-dev/noq publishes `moq-noq-proto`,
  `moq-noq`, and `moq-noq-udp`, tracks its parent, and the sync procedure is
  written down
- [Deliver the application close before io_uring teardown](/quest/m2/quic/uring-close.md) -
  the peer receives the final close when the client immediately stops its worker
- [ECN on the io_uring UDP path](/quest/m2/quic/ecn-uring.md) - the ring's
  sends carry ECT(0) and its receives read the mark, matching `noq-udp`
- [Measure ECN on the backbone](/quest/m2/quic/ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [Per-stream ACK progress](/quest/m2/quic/ack-progress.md) - the fork reports
  how far a send stream has been acknowledged and when
- [poll_acked in web-transport](/quest/m2/quic/ack-hook.md) - the
  backend-neutral hook that awaits an acknowledged stream offset, implemented
  for noq and released
- [Reliable stream reset](/quest/m2/quic/reliable-reset.md) - `RESET_STREAM_AT`,
  so a reset WebTransport stream still delivers its header
- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - strict
  subscription priority, fair buckets, and newest-first group order replace
  the lossy scalar; retransmits follow the same order
- [Keep-alive by deadline](/quest/m2/quic/keep-alive.md) - a PING only when
  the idle deadline nears, no fixed timer
- [Relay peers get wider limits](/quest/m2/quic/peer-limits.md) - MAX_STREAMS
  and MAX_DATA are raised after SETUP identifies a cluster peer
- [Per-stream deadlines](/quest/m2/quic/deadline.md) - hopeless retransmits
  become resets, and a tail loss probe fires early while there is still time
- [Probe by early retransmission](/quest/m2/quic/probe.md) - measure capacity
  with useful retransmissions instead of padding
- [qmux on the QUIC stream state machine](/quest/m2/quic/qmux.md) - qmux is a
  first-class crate in the fork over the shared stream state machine
- [Careful resume on reconnect](/quest/m2/quic/careful-resume.md) - a redial
  starts at the previous connection's rate
- [L4S on the backbone](/quest/m2/quic/ecn.md) - an ECT(1) option in the
  fork, an `ecn` config knob, and a dualpi2 measurement
- [Release the stack](/quest/m2/quic/release.md) - publish immutable,
  consumable versions of the fork and its adapters
- [Upstream the fork](/quest/m2/quic/upstream.md) - every general carried
  change is offered to n0-computer/noq once its shape has settled

## Related

- [Scope track priority](/quest/m2/track-priority-scope.md) - the
  per-broadcast fairness policy on cluster sessions
- [Starvation](/quest/m2/qos/starvation.md) - the first consumer of ACK
  progress: how far behind viewers are, from the relay's point of view
- [Receive timestamps](/quest/m3/quic-receive-ts.md) - per-packet arrival
  times for GCC and deadlines
- [GCC egress experiment](/quest/m3/quic-gcc.md) - a measured verdict on
  WebRTC-style delay control
- [FEC experiment](/quest/m3/quic-fec.md) - a measured verdict on transport
  redundancy
- [Kernel pacing](/quest/m3/quic-kernel-pacing.md), [Send batching](/quest/m3/quic-send-batching.md),
  [Send buffer pools](/quest/m3/quic-buffer-pool.md), [BBR3 app-limited](/quest/m3/quic-bbr-app-limited.md) -
  the syscall, allocation, and controller spikes
- [Multipath spike](/quest/m3/multipath-spike.md) - a noq capability that
  MoQ does not use yet
