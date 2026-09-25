# Own the QUIC stack

## Goal

MoQ owns the QUIC features it needs. noq, the Quinn-derived stack n0
maintains for Iroh, is the parent; the moq-dev fork carries what MoQ needs on
MoQ's schedule and offers it upstream when it is general. One core serves the
tokio backend, the thread-per-core `moq-uring` backend, iroh, and qmux. The
features are per-stream acknowledgment progress, reliable stream resets,
hierarchical stream scheduling with per-broadcast fairness, the shared stream
state machine used by qmux, capacity probing for media, per-stream
deadlines, deadline-based and wider limits for relay peers. The experiments that may join them (GCC, FEC, receive
timestamps, kernel pacing, buffer pools, probing, L4S, careful resume) live
in [m2](/quest/m2/README.md).

## Plan

Everything here ships from moq-dev/noq, the fork MoQ publishes as `moq-noq*`.
One stack carries every change on MoQ's own QUIC paths; a build with the `iroh`
feature also compiles upstream noq, and iroh connections are outside what these
quests reach.

The seven BBR correctness fixes follow the fork bootstrap. They are separate
PRs, but one owner should work in the shared controller code at a time.
Their controller-level regressions extend the shared test `Sim` in
`bbr3/mod.rs` with only what each needs, rather than adding another
simulation loop; a fix at the transport boundary still needs a transport
test through the real callbacks. The existing loops stay, since the fork
merges upstream weekly and a port would conflict.
The [BBR release](/quest/m1/quic/bbr-release.md) delivers them without waiting
for the remaining transport features. The
[Google comparison](/quest/m2/quic-bbr-google.md) is a separate study.

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

- [Recalibrate BBR startup pacing from measured RTT](/quest/m1/quic/bbr-startup-pacing.md) - measured RTT replaces the nominal startup rate for media senders
- [Protect bandwidth samples during BBR ProbeRTT](/quest/m1/quic/bbr-probe-rtt.md) - intentionally reduced sending cannot masquerade as reduced capacity
- [Preserve BBR state across a spurious loss episode](/quest/m1/quic/bbr-loss-undo.md) - consecutive losses preserve the original recovery snapshot
- [Release BBR fixes](/quest/m1/quic/bbr-release.md) - publish and pin the corrected controller independently of later features
- [Align BBR loss handling with draft-06](/quest/m1/quic/bbr-loss-parity.md) - losses use their own sample and undo re-enters ProbeUp through Refill
- [Mark BBR starvation wherever the source runs dry](/quest/m1/quic/bbr-app-limited-edges.md) - partial polls count, local send caps do not, receiver credit is pinned
- [Deliver the application close before io_uring teardown](/quest/m1/quic/uring-close.md) -
  the peer receives the final close when the client immediately stops its worker
- [Measure ECN on the backbone](/quest/m1/quic/ecn-measure.md) - a written
  verdict on marking versus dropping, and whether Linode and OVH keep marks
- [Per-stream ACK progress](/quest/m1/quic/ack-progress.md) - the fork reports
  how far a send stream has been acknowledged and when
- [poll_acked in web-transport](/quest/m1/quic/ack-hook.md) - the
  backend-neutral hook that awaits an acknowledged stream offset, implemented
  for noq and released
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - `RESET_STREAM_AT`,
  so a reset WebTransport stream still delivers its header
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - strict
  subscription priority, fair buckets, and newest-first group order replace
  the lossy scalar; retransmits follow the same order
- [Relay peers get wider limits](/quest/m1/quic/peer-limits.md) - MAX_STREAMS
  and MAX_DATA are raised after SETUP identifies a cluster peer
- [Per-stream deadlines](/quest/m1/quic/deadline.md) - hopeless retransmits
  become resets, and a tail loss probe fires early while there is still time
- [qmux on the QUIC stream state machine](/quest/m1/quic/qmux.md) - qmux is a
  first-class crate in the fork over the shared stream state machine
- [Release the stack](/quest/m1/quic/release.md) - publish immutable,
  consumable versions of the fork and its adapters
- [Upstream the fork](/quest/m1/quic/upstream.md) - every general carried
  change is offered to n0-computer/noq once its shape has settled

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - the
  per-broadcast fairness policy on cluster sessions
- [Starvation](/quest/m1/qos/starvation.md) - the first consumer of ACK
  progress: how far behind viewers are, from the relay's point of view
- [Receive timestamps](/quest/m2/quic-receive-ts.md) - per-packet arrival
  times for GCC and deadlines
- [GCC egress experiment](/quest/m2/quic-gcc.md) - a measured verdict on
  WebRTC-style delay control
- [FEC experiment](/quest/m2/quic-fec.md) - a measured verdict on transport
  redundancy
- [Kernel pacing](/quest/m2/quic-kernel-pacing.md), [Send batching](/quest/m2/quic-send-batching.md),
  [Send buffer pools](/quest/m2/quic-buffer-pool.md), [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) -
  the syscall, allocation, and controller spikes
- [Multipath spike](/quest/m2/multipath-spike.md) - a noq capability that
  MoQ does not use yet
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [L4S on the backbone](/quest/m2/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/m2/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive by deadline](/quest/m2/quic-keep-alive.md) - a PING only when the idle deadline nears, no fixed timer
