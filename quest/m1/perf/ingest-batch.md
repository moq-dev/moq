# [S] Batch the relay ingest write path

## Goal

Relay ingest pays a full group mutex acquisition, a waiter-list wake fanout,
and a real clock read for every received QUIC chunk. Egress is already
amortized (the `Prefetch` refills eight frames under one lock and stamps the
charge and stats once per batch); ingest has no equivalent. Make a burst of
received chunks pay one lock, wake, and clock cycle.

## Plan

Mechanism, per the 2026-09 survey:

- Both wire ingests drain a frame's payload through
  `coding::Reader::poll_read_frame`. The group lock, the charge clock read, and
  the waiter drain used to run per chunk; they now run once at the poll boundary
  and once at `frame_commit`.
- The batched machinery exists but is unused here:
  `group::Producer::write_frames` takes a `frame::Buffer` and pays one lock
  per batch (benched at roughly 5x for N=8 in `rs/moq-net/benches/group.rs`),
  but the ingest path streams chunks through `create_frame_owned` instead.

Remaining:

- Where whole frames are available in one poll turn, feed them through
  `write_frames`/`frame::Buffer` instead of frame-at-a-time creation. This is
  the larger half: a small frame that arrives whole still pays a
  `create_frame_owned` plus a `frame_commit`, two lock acquisitions where the
  batch API pays one for the whole burst.
- The per-chunk `stats` bumps on the same loop, which `write` still pays
  individually.

Done: the notification cadence. `coding::Reader::poll_read_frame` owns the
payload drain for both wire ingests, so the wake happens once where the loop
yields (or once per `WAKE_BUDGET` bytes, since transport readiness alone is not
a bound on time), and `frame_commit` restarts the retention clock so the
deferral can never lose a stamp.

Acceptance: ingest CPU per Gbps on the video shape and the chat shape
(`just bench BASE` on Linux), plus `rs/moq-net/benches/group.rs`. Frame
delivery latency at the live edge must not regress.
