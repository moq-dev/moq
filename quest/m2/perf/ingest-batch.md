# [M] Batch the relay ingest write path

## Goal

Relay ingest pays two group lock acquisitions (`create_frame_owned`, then
`frame_commit`), a waiter-list wake, and a clock read for every received
frame. A frame's payload chunks are already amortized per poll, but a burst of
small whole frames is not. Egress is already amortized (the `Prefetch`
refills eight frames under one lock and stamps the charge and stats once per
batch). Make a burst of whole frames received in one poll turn pay one lock,
wake, and clock cycle.

## Plan

Where the path stands:

- Both wire ingests drain a frame's payload through
  `coding::Reader::poll_read_frame` (`rs/moq-net/src/coding/reader.rs`).
  The group lock, the charge clock read, and the waiter drain run once at the
  poll boundary, or once per `WAKE_BUDGET` bytes (since transport readiness
  alone is not a bound on time), and once at `frame_commit`
  (`rs/moq-net/src/model/group.rs`), which restarts the retention clock so
  the deferral can never lose a stamp.
- The batched machinery exists but is unused here:
  `group::Producer::write_frames` takes a `frame::Buffer` and pays one lock
  per batch, while the ingest path (lite's `FrameIngest`, the IETF
  subscriber's object loop) creates each frame through `create_frame_owned`
  instead. The `group_write_frames` bench in `rs/moq-net/benches/group.rs`
  compares the two as `single` against `batch32`, a `frame::Buffer::<32>`
  flushed through `write_frames`; take the before number from there.

Remaining (decided 2026-10-08: the Goal is per frame, since per-chunk costs
are already amortized):

- Where whole frames are available in one poll turn, feed them through
  `write_frames`/`frame::Buffer` instead of frame-at-a-time creation. This is
  the larger half: a small frame that arrives whole still pays a
  `create_frame_owned` plus a `frame_commit`, two lock acquisitions where the
  batch API pays one for the whole burst.
- The per-chunk `stats` bumps on the same loop, which `write` still pays
  individually.

Acceptance: ingest CPU per Gbps on the video shape and the chat shape
(`just bench BASE` on Linux), plus `group_write_frames` in
`rs/moq-net/benches/group.rs`. Frame delivery latency at the live edge must
not regress.
