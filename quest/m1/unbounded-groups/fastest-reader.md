# [M] Fastest-reader pacing

## Goal

Every hop buffers at most a window of a group past its fastest reader, then
stops reading the upstream QUIC stream, so stream flow control pushes back hop
by hop to the publisher. A hop with no reader keeps reading and evicting, so
pacing never blocks an unwatched track. A reader that falls further behind than
the window refills when a fill handler exists and otherwise ends with
`Error::Evicted`. A byte stream over MoQ then loses nothing to a reader that
keeps up, and each hop keeps its own congestion control and retransmissions.

## Plan

Decided 2026-10-25 (maintainer, mesh planning interview), reopening the
2026-10-24 "the live edge is never paced by a reader" decision for all tracks:

- Pace by the fastest reader, never the slowest: one slow reader must not stall
  the publisher or its other readers. Rejected: pacing by the slowest reader
  (TCP fanout semantics), and an opt-in flow-controlled track flag.
- Propagate with QUIC stream flow control per hop. No new wire messages, so it
  lands in the m1 cut beside [Front eviction](/quest/m1/unbounded-groups/front-eviction.md).
  Rejected: explicit MoQ-level credit messages.
- A reader past the window gets no new mechanism: it is an ordinary front
  eviction victim. Tracks without a fill handler, such as
  [byte streams](/quest/m2/stream/README.md), end that reader with
  `Evicted`, which a TCP proxy surfaces as a reset.

Open, for the implementer to measure and record: the window size (per group
or per hop, and whether it derives from the QUIC stream receive window), and
how an origin producer observes pacing. A relay always paces; whether an origin
write waits, or a readiness future lets producers that can wait (byte streams)
await while live encoders keep writing, is an API decision to bring to the
maintainer before coding. qmux transports carry the same rule through their
own stream credit ([qmux receive lease](/quest/m2/qmux-receive-lease.md)).

Tests: a relay chain where a fast reader paces a publisher that writes faster
than the link, with memory held near the window; a second, slower reader ending
`Evicted` without slowing the fast one; a track with no readers never blocking
its publisher; Rust and JS parity. Benchmark the hot path in `group.rs` and
record any regression.

Docs: the moq-lite concept page's congestion section and the relay cache docs
change inline.

## Required

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - the eviction this pacing bounds
