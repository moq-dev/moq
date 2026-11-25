# [L] Decode-order timestamps: moq-timestamp-03 Object IDs and unsigned lite deltas

## Goal

Frame timestamps become decode-order time end to end, with presentation time
carried by the hang `cts` container:

- On moq-transport draft-23 and later, a timed track declares `ID_TIMESCALE`
  (`0x40B70`) and each object's Object ID encodes its decode timestamp
  (`DTS = Group ID + Object ID - TIMESTAMP_TIE`, with `TIMESTAMP_TIE`
  `0x40B72` omitted when zero), per draft-lcurley-moq-timestamp-03.
- moq-lite carries unsigned, non-decreasing `FRAME` timestamp deltas instead
  of zigzag signed ones, with `GROUP_END`'s delta measured from the last frame.
- First-party encoders emit the `cts` container, so B-frame streams keep their
  presentation times while the transport timestamp stays monotonic.

A relay serves every frame with the same Object ID and timestamp whichever
copy it holds, across dense and sparse peers and across failover.

## Plan

Split out of [moq-dev/moq#9553](https://github.com/moq-dev/moq/pull/9553)
(maintainer, 2026-11-25), which keeps the draft reviews and the `raw`/`cts`
decoders. The first implementation, with three review rounds of fixes, is on
branch `t3/moqt-timestamp-ids` at `80f1cbb7105b5a2be9bb47fce5454092e1323a80`
(it passed `just check` and `just test interop --all` there). Start from it
rather than from scratch, but revisit its shape first: its open findings all
come from per-copy timing state.

- Each cached copy derives `timed_oid` and a clamped `mono_ts` as frames
  enter it (`assign_timing` in `rs/moq-net/src/model/group.rs`), so a copy
  that starts mid-group or holds a tied frame first computes values the
  origin never sent. Carry the received wire Object ID and timestamp through
  instead of recomputing them, or refuse to serve an offset copy in the other
  ID mode.
- Open review findings at that head: Codex round 3 (relay Object ID
  translation by downstream mode, Largest Object translation, standalone
  `END_OF_TRACK` caps in Rust and JS, partial-frame resume across dense and
  sparse routes, gap property renumbering, bounded terminal lookup), Claude
  round 3 (Largest Object reported in the upstream's ID scheme), and
  CodeRabbit on `rs/moq-net/src/ietf/publisher.rs`,
  `rs/moq-net/src/model/group.rs`, `rs/moq-net/src/ietf/properties.rs`,
  `rs/moq-net/src/lite/publisher.rs`, `js/net/src/group.ts`,
  `js/net/src/wire.ts`, and `drafts/draft-lcurley-moq-ietf.md` (backward
  timestamps across the hop). The PR's review threads list each one.
- The Draft-18..22 shim that keeps legacy `TIMESCALE` (`0x08`) and dense
  Object IDs for mixed-version rollouts stays, marked for deletion at the next
  bump.
- Update `drafts/draft-lcurley-moq-timestamp.md`, `drafts/draft-lcurley-moq-ietf.md`,
  `drafts/draft-lcurley-moq-e2ee.md` (frame index recovery for partial sparse
  delivery), `drafts/draft-lcurley-moq-lite.md` (FRAME and GROUP_END), the
  code point table in `drafts/README.md`, `doc/concept/moq-lite.md`, and
  `doc/concept/standard.md` with the code, as the branch already does.

## Waiting

- Maintainer: which lite version carries unsigned decode-order deltas. Options: lite-07 before its freeze, adding this to [Finalize moq-lite-07](/quest/m2/lite07-finalize.md)'s Required; or the next wip version after the cut, together with the `cts` encoder switch. Recommended: the next wip version, since lite-07's freeze should not wait on a timing redesign, and the deltas only pay off once encoders send `cts`.
- Maintainer: how a backwards timestamp is handled under unsigned deltas. Options: the producer refuses a frame whose timestamp is below its predecessor's, where the frame is created; or egress clamps it up to the previous timestamp, as the branch does. Recommended: refuse at the producer once encoders send `cts`, which removes the per-copy clamp state behind the open findings; clamping silently rewrites B-frame presentation times.
