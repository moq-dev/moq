# Own the QUIC stack

## Goal

MoQ owns the QUIC features it needs. noq, the Quinn-derived stack n0
maintains for Iroh, is the parent; the moq-dev fork carries what MoQ needs on
MoQ's schedule and offers it upstream when it is general. One core serves the
tokio backend, the thread-per-core `moq-uring` backend, iroh, and qmux. The
features are BBR correctness, reliable stream resets, hierarchical stream
scheduling with per-broadcast fairness, and wider limits for relay peers.
Per-stream acknowledgment progress, per-stream deadlines, qmux on the shared
stream state machine, and the experiments (GCC, receive timestamps, the egress
profile, media probing, L4S, careful resume, deadline keep-alive) live in
[m2](/quest/m2/README.md) and do not gate this line.

## Plan

Everything here ships from moq-dev/noq, the fork MoQ publishes as `moq-noq*`.
One stack carries every change on MoQ's own QUIC paths; a build with the `iroh`
feature also compiles upstream noq, and iroh connections are outside what these
quests reach.

The seven BBR correctness fixes shipped in moq-noq 1.3.1 (#4206). The
remaining BBR quests here and [BBR ACK cleanup](/quest/m1/bbr-ack-cleanup.md)
all edit `bbr3/mod.rs`, so one owner should work there at a time.
Controller-level regressions extend the shared test `Sim` in `bbr3/mod.rs`
with only what each needs, rather than adding another simulation loop; a fix
at the transport boundary still needs a transport test through the real
callbacks. The existing loops stay, since the fork merges upstream weekly and
a port would conflict. Each BBR fix ships in a fork patch release without
waiting for the remaining transport features. The comparison against
Google's BBR is part of the separate
[natural drain study](/quest/m2/quic-bbr-natural-drain.md).

Rules the line keeps:

- a carried change lists its upstream PR or the reason it has none;
- the fork's packages are `moq-noq-proto`, `moq-noq`, and `moq-noq-udp`,
  never a crate that impersonates the parent;
- published MoQ crates depend on crates.io releases of the fork, never a
  workspace-only Cargo patch or a mutable branch;
- each feature quest releases the fork crate it changes and records the
  parent noq commit it is based on; there is no separate release quest;
- MoQ's config names congestion families (`Loss`, `Delay`, and `RealTime`
  once GCC ships), never algorithms; noq's public `Controller` trait is the
  seam experiments plug into, and MoQ owns which algorithm each family means.

The scheduling contract has three levels: strict subscription priority,
byte-fair service between send groups at the same priority, then the
subscription's chosen group order within its own bucket. On a relay-to-relay
session with fairness enabled, the send group is the broadcast. The default
MoQ order is newest group first; an ordered subscription keeps oldest first.
This is a transport API change, not a MoQ wire change.

Decided in the 2026-09-30 audit: deadlines, qmux, BBR loss parity, ECN
measurement, ACK progress, and the ACK hook moved to m2, since no m1 quest
consumes them. The release quest was deleted: the fork already publishes to
crates.io (moq-noq 1.3.2, web-transport-moq 2.0.0) with no workspace patch,
and the qmux crate release folds into [qmux](/quest/m2/quic-qmux.md).

## Required

- [BBR idle burst](/quest/m1/quic/bbr-app-limited.md) - a fork regression proves a burst after a long idle is paced at the learned bandwidth, closing #4219
- [Mark BBR starvation wherever the source runs dry](/quest/m1/quic/bbr-app-limited-edges.md) - partial polls count, local send caps do not, receiver credit is pinned
- [Deliver the application close before io_uring teardown](/quest/m1/quic/uring-close.md) -
  the peer receives the final close when the client immediately stops its worker
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - `RESET_STREAM_AT`,
  so a reset WebTransport stream still delivers its header
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - strict
  subscription priority, fair buckets, and newest-first group order replace
  the lossy scalar; retransmits follow the same order
- [Relay peers get wider limits](/quest/m1/quic/peer-limits.md) - MAX_STREAMS
  and MAX_DATA are raised after SETUP identifies a cluster peer
- [Upstream the fork](/quest/m1/quic/upstream.md) - every general carried
  change is offered to n0-computer/noq once its shape has settled

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - the
  per-broadcast fairness policy on cluster sessions
- [Starvation](/quest/m1/qos/starvation.md) - the first consumer of ACK
  progress: how far behind viewers are, from the relay's point of view
- [Multipath spike](/quest/m2/multipath-spike.md) - a noq capability that
  MoQ does not use yet
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [L4S on the backbone](/quest/m2/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/m2/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive by deadline](/quest/m2/quic-keep-alive.md) - a PING only when the idle deadline nears, no fixed timer
- [Receive timestamps](/quest/m3/quic-receive-ts.md) - per-packet arrival
  times for GCC and deadlines
- [GCC egress experiment](/quest/m3/quic-gcc.md) - a measured verdict on
  WebRTC-style delay control
