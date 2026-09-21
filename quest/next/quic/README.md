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
timestamps, kernel pacing, buffer pools) live in next.

## Plan

Everything here assumes the single noq stack. The [fork](/quest/next/quic/fork.md)
is the first quest in the line and most others require it.

The six BBR correctness fixes follow the fork bootstrap. They are separate
PRs, but one owner should work in the shared controller code at a time.
The [BBR release](/quest/next/quic/bbr-release.md) delivers them without waiting
for the remaining transport features. The
[Google comparison](/quest/future/quic-bbr-google.md) is a separate study.

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

- [Fork noq](/quest/next/quic/fork.md) - moq-dev/noq publishes `moq-noq-proto`,
  `moq-noq`, and `moq-noq-udp`, tracks its parent, and the sync procedure is
  written down
- [Preserve QUIC packet identity in BBR](/quest/next/quic/bbr-packet-identity.md) - ACKs and losses identify the right packet across QUIC spaces
- [Finish each BBR ACK sample before using it](/quest/next/quic/bbr-ack-sampling.md) - current delivery samples reach the model once with consistent metadata
- [Finish BBR bandwidth-probe feedback once](/quest/next/quic/bbr-probe-feedback.md) - cruise rounds neither age probe history repeatedly nor retain probe-loss classification
- [Recalibrate BBR startup pacing from measured RTT](/quest/next/quic/bbr-startup-pacing.md) - measured RTT replaces the nominal startup rate for media senders
- [Protect bandwidth samples during BBR ProbeRTT](/quest/next/quic/bbr-probe-rtt.md) - intentionally reduced sending cannot masquerade as reduced capacity
- [Preserve BBR state across a spurious loss episode](/quest/next/quic/bbr-loss-undo.md) - consecutive losses preserve the original recovery snapshot
- [Release BBR fixes](/quest/next/quic/bbr-release.md) - publish and pin the corrected controller independently of later features
- [Deliver the application close before io_uring teardown](/quest/next/quic/uring-close.md) -
  the peer receives the final close when the client immediately stops its worker
- [Measure ECN on the backbone](/quest/next/quic/ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [Per-stream ACK progress](/quest/next/quic/ack-progress.md) - the fork reports
  how far a send stream has been acknowledged and when
- [poll_acked in web-transport](/quest/next/quic/ack-hook.md) - the
  backend-neutral hook that awaits an acknowledged stream offset, implemented
  for noq and released
- [Reliable stream reset](/quest/next/quic/reliable-reset.md) - `RESET_STREAM_AT`,
  so a reset WebTransport stream still delivers its header
- [Hierarchical stream scheduling](/quest/next/quic/scheduler.md) - strict
  subscription priority, fair buckets, and newest-first group order replace
  the lossy scalar; retransmits follow the same order
- [Keep-alive by deadline](/quest/next/quic/keep-alive.md) - a PING only when
  the idle deadline nears, no fixed timer
- [Relay peers get wider limits](/quest/next/quic/peer-limits.md) - MAX_STREAMS
  and MAX_DATA are raised after SETUP identifies a cluster peer
- [Per-stream deadlines](/quest/next/quic/deadline.md) - hopeless retransmits
  become resets, and a tail loss probe fires early while there is still time
- [Probe by early retransmission](/quest/next/quic/probe.md) - measure capacity
  with useful retransmissions instead of padding
- [qmux on the QUIC stream state machine](/quest/next/quic/qmux.md) - qmux is a
  first-class crate in the fork over the shared stream state machine
- [Careful resume on reconnect](/quest/next/quic/careful-resume.md) - a redial
  starts at the previous connection's rate
- [L4S on the backbone](/quest/next/quic/ecn.md) - an ECT(1) option in the
  fork, an `ecn` config knob, and a dualpi2 measurement
- [Release the stack](/quest/next/quic/release.md) - publish immutable,
  consumable versions of the fork and its adapters
- [Upstream the fork](/quest/next/quic/upstream.md) - every general carried
  change is offered to n0-computer/noq once its shape has settled

## Related

- [Scope track priority](/quest/next/track-priority-scope.md) - the
  per-broadcast fairness policy on cluster sessions
- [Starvation](/quest/next/qos/starvation.md) - the first consumer of ACK
  progress: how far behind viewers are, from the relay's point of view
- [Receive timestamps](/quest/future/quic-receive-ts.md) - per-packet arrival
  times for GCC and deadlines
- [GCC egress experiment](/quest/future/quic-gcc.md) - a measured verdict on
  WebRTC-style delay control
- [FEC experiment](/quest/future/quic-fec.md) - a measured verdict on transport
  redundancy
- [Kernel pacing](/quest/future/quic-kernel-pacing.md), [Send batching](/quest/future/quic-send-batching.md),
  [Send buffer pools](/quest/future/quic-buffer-pool.md), [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) -
  the syscall, allocation, and controller spikes
- [Multipath spike](/quest/future/multipath-spike.md) - a noq capability that
  MoQ does not use yet
