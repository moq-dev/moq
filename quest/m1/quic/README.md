# Own the QUIC stack

## Goal

MoQ owns the QUIC features it needs in an in-tree stack, `moq-quic`, hard
forked from quinn (see [the fork](/quest/m1/quic/fork/README.md) for why
quinn and not noq). One core serves the tokio backend, the thread-per-core
`moq-uring` backend, and qmux; iroh stays on upstream noq. The features are
BBR correctness, reliable stream resets, hierarchical stream scheduling with
per-broadcast fairness, wider limits for relay peers, and endpoint sharding.
Per-stream acknowledgment progress, per-stream deadlines, qmux on the shared
stream state machine, and the experiments (GCC, receive timestamps, the egress
profile, media probing, L4S, careful resume, deadline keep-alive) live in
[m2](/quest/m2/README.md) and do not gate this line.

## Plan

Decided in the 2026-09-30 plan: the moq-dev/noq soft fork is replaced by a
hard fork of quinn in `rs/`, so every QUIC change here waits for
[the fork](/quest/m1/quic/fork/README.md) and lands on `dev`. Retarget this
line's PR (#3975) to `dev` when the fork starts. moq-dev/noq is frozen to
security patches for `main`.

Older quests say "the fork" or "noq"; read that as `moq-quic`. Their steps to
publish a fork release, pin it, or offer a change upstream are superseded:
a change lands in-tree with its consumer, and upstreaming is optional.

The seven BBR correctness fixes shipped in moq-noq 1.3.1 (#4206) and move to
`moq-quic` with the [BBR3 port](/quest/m1/quic/fork/bbr3.md). The remaining
BBR quests here and [BBR ACK cleanup](/quest/m1/bbr-ack-cleanup.md) all edit
`bbr3/mod.rs`, so one owner should work there at a time. Controller-level
regressions extend the shared test `Sim` in `bbr3/mod.rs` with only what each
needs, rather than adding another simulation loop; a fix at the transport
boundary still needs a transport test through the real callbacks. The
comparison against Google's BBR is part of the separate
[natural drain study](/quest/m2/quic-bbr-natural-drain.md).

MoQ's config names congestion families (`Loss`, `Delay`, and `RealTime` once
GCC ships), never algorithms; the `Controller` trait is the seam experiments
plug into, and MoQ owns which algorithm each family means.

The scheduling contract has three levels: strict subscription priority,
byte-fair service between send groups at the same priority, then the
subscription's chosen group order within its own bucket. On a relay-to-relay
session with fairness enabled, the send group is the broadcast. The default
MoQ order is newest group first; an ordered subscription keeps oldest first.
This is a transport API change, not a MoQ wire change.

Decided in the 2026-09-30 audit: deadlines, qmux, BBR loss parity, ECN
measurement, ACK progress, and the ACK hook moved to m2, since no m1 quest
consumes them.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - quinn in-tree as `moq-quic`, with BBR3 and lazy stream slots, and MoQ switched onto it
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
- [Shard the endpoint](/quest/m1/quic/shard.md) - the library shards a
  server endpoint across cores by default, replacing the `--workers` group
- [Surface UDP send errors](/quest/m1/quic/send-errors.md) - a client drops a QUIC attempt the moment its address family is unreachable

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - the
  per-broadcast fairness policy on cluster sessions
- [Starvation](/quest/m1/qos/starvation.md) - the first consumer of ACK
  progress: how far behind viewers are, from the relay's point of view
- [Discover media headroom](/quest/m2/quic-probe.md) - test useful-media pacing before adding redundant probe traffic
- [L4S on the backbone](/quest/m2/quic-ecn.md) - an ECT(1) option in the fork, an `ecn` config knob, and a dualpi2 measurement
- [Careful resume on reconnect](/quest/m2/quic-careful-resume.md) - a redial starts at the previous connection's rate
- [Keep-alive by deadline](/quest/m2/quic-keep-alive.md) - a PING only when the idle deadline nears, no fixed timer
- [GCC egress experiment](/quest/m3/quic-gcc.md) - receive timestamps and a measured verdict on
  WebRTC-style delay control
