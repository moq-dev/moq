# Own the QUIC stack

## Goal

MoQ owns the QUIC features it needs in an in-tree stack, `moq-quic`, hard
forked from quinn rather than noq, whose multipath, NAT traversal, and QAD
MoQ never negotiates. One core serves the tokio backend, the thread-per-core
`moq-uring` backend, and qmux; iroh stays on upstream noq. The features are
BBR correctness, reliable stream resets, hierarchical stream scheduling with
per-broadcast fairness, bounded incomplete-message memory, wider limits for
relay peers, and endpoint sharding.
Per-stream acknowledgment progress, per-stream deadlines, qmux on the shared
stream state machine, and the experiments (the egress profile, ECN
measurement, deadline keep-alive) live in [m2](/quest/m2/README.md); media
probing is its own [m2 quest](/quest/m2/quic-probe.md); L4S,
careful resume, and a receive-timestamps spike toward GCC live in
[m3](/quest/m3/README.md). None of them gate this line.

## Plan

Decided in the 2026-09-30 plan: the moq-dev/noq soft fork is replaced by a
hard fork of quinn in `rs/`, where every QUIC change here lands.
moq-dev/noq is frozen to security patches for `release` and archived once
no released MoQ crate depends on it.

The seven BBR correctness fixes shipped in moq-noq 1.3.1 (#4206) are in
`moq-quic`'s BBR3. The remaining BBR quests here all edit
`bbr3/mod.rs`, so one owner should work there at a time. Controller-level
regressions extend the shared test `Sim` in `bbr3/mod.rs` with only what each
needs, rather than adding another simulation loop; a fix at the transport
boundary still needs a transport test through the real callbacks. The
comparison against Google's BBR is part of the separate
[natural drain study](/quest/m2/quic-bbr-natural-drain.md).

MoQ's config names congestion families (`Loss`, `Delay`, and `RealTime` once
GCC ships), never algorithms; the `Controller` trait is the seam experiments
plug into, and MoQ owns which algorithm each family means.

With fairness enabled, scheduling first shares bytes between broadcast
domains. Inside each domain, subscription priority is strict, equal-priority
subscriptions share bytes fairly, and each subscription applies its group
order. Every subscription has a send group; the broadcast domain is the tier
above it, not the send group itself. Without domain fairness, retain strict
priority in the configured domain. The default
MoQ order is newest group first; an ordered subscription keeps oldest first.
Datagrams join their subscription's send group in its group order, ahead
of a stream of the same group.
This is a transport API change, not a MoQ wire change.

Decided in the 2026-09-30 audit: deadlines, qmux, BBR loss parity, ECN
measurement, ACK progress, and the ACK hook moved to m2, since no m1 quest
consumes them. Decided 2026-10-08: L4S and careful resume moved on to m3,
and GCC shrank to a receive-timestamps spike, since none has a consumer.

Decided in the 2026-10-10 receive-memory plan: native message assembly holds
connection credit until complete-unit handoff, while stream credit advances
on reads with a very large configurable default. MoQ recovers under assembly
pressure with receiver STOP_SENDING, including against JavaScript senders.
The memory allowance must fit the largest supported encoded unit. Control
streams have highest priority but are not immune to cancellation. Completed
cache limits remain separate; bitrate pacing composes with this memory cap.

## Required

- [Regression net](/quest/m1/quic/coverage/README.md) - benchmarks, fuzzing, characterization tests, and a seeded simulation, so later QUIC changes see every behavior or cost change
- [Bound message assembly](/quest/m1/quic/receive-memory.md) - complete headers and frames return credit, and receiver priority cancellation restores progress
- [A reset group keeps its header](/quest/m1/quic/group-reset.md) - a lite
  publisher resets a group with `RESET_STREAM_AT`, so its subscriber drops the
  tail grace
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - strict
  subscription priority, fair buckets, and newest-first group order replace
  the lossy scalar; retransmits follow the same order
- [Schedule datagrams by subscription priority](/quest/m1/quic/datagram-priority.md) -
  datagrams join their subscription's send group instead of preempting every stream
- [Relay peers get wider limits](/quest/m1/quic/peer-limits.md) - MAX_STREAMS
  and MAX_DATA are raised after SETUP identifies a cluster peer
- [Delete NewReno](/quest/m1/quic/new-reno.md) - the unused public controller goes

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - the
  per-broadcast fairness policy on cluster sessions
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [L4S on the backbone](/quest/m3/quic-ecn.md) - an ECT(1) option in `moq-quic`, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/m3/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive from the idle timeout](/quest/m2/quic-keep-alive.md) - the keep-alive interval defaults to a fraction of the negotiated idle timeout; an explicit `quic.keep_alive` overrides
- [QUIC receive-timestamps spike](/quest/m3/quic-gcc.md) - measures whether receive timestamps give a cheaper, more accurate forward delay than half the RTT
