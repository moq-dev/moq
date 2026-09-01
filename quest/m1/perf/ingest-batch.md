# [M] Batch the relay ingest write path

## Goal

Relay ingest pays a full group mutex acquisition, a waiter-list wake fanout,
and a real clock read for every received QUIC chunk. Egress is already
amortized (the `Prefetch` refills eight frames under one lock and stamps the
charge and stats once per batch); ingest has no equivalent. Make a burst of
received chunks pay one lock, wake, and clock cycle.

## Plan

Mechanism, per the 2026-09 survey:

- `lite::Subscriber`'s group receive loop reads a chunk from the transport
  and calls `frame::Producer` (`Raw::write`), which calls
  `group::Producer::frame_notify` after every chunk. `frame_notify` takes
  the group lock, records the write on the charge (which reads
  `model::clock::now()` and bumps the process-global pool atomics), drains
  `waiters_value`, and wakes every parked consumer.
- The batched machinery exists but is unused here:
  `group::Producer::write_frames` takes a `frame::Buffer` and pays one lock
  per batch (benched at roughly 5x for N=8 in `rs/moq-net/benches/group.rs`),
  but the ingest path streams chunks through `create_frame_owned` instead.

Proposal:

- Where whole frames are available in one poll turn, feed them through
  `write_frames`/`frame::Buffer` instead of frame-at-a-time creation.
- For streamed chunks inside one frame, coalesce the notify: keep reading
  chunks while the transport is ready and issue one `frame_notify` when the
  read loop would block (or at a byte budget), so a burst is one
  lock-plus-wake. The payload copy into `FrameBuf` is already lock-free;
  only the notification cadence changes.
- Amortize the per-chunk charge stamp the same way: one `touch` per
  coalesced notify, not one per chunk.
- Semantics to preserve: a consumer parked mid-frame must still observe
  progress promptly; bound the coalescing window by the transport's own
  readiness (never time), so latency added is zero when the socket is the
  bottleneck.

Acceptance: ingest CPU per Gbps on the video shape and the chat shape
(`just bench BASE` on Linux), plus `rs/moq-net/benches/group.rs`. Frame
delivery latency at the live edge must not regress.
