# [L] moq-timestamp-03 Object IDs and unsigned lite timestamp deltas

## Goal

The timestamp work split out of the 0.22 draft review lands:

- On moq-transport draft-23 and later, a timed track declares `ID_TIMESCALE`
  (`0x40B70`) and each object's Object ID encodes its timestamp
  (`Group ID + Object ID - TIMESTAMP_TIE`, with `TIMESTAMP_TIE` `0x40B72`
  omitted when zero), per draft-lcurley-moq-timestamp-03.
- moq-lite carries unsigned `FRAME` timestamp deltas instead of zigzag signed
  ones, so a Group's timestamps never decrease, with `GROUP_END`'s delta
  measured from the last frame.
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
rather than from scratch, but revisit its shape first. Its open review
findings, listed in that PR's review threads and its `## Review` history, all
come from timing derived per cached copy: a copy that starts mid-group or
holds a tied frame first computes Object IDs and timestamps the origin never
sent. A copy must serve the received wire values, or refuse to serve what it
cannot prove matches them.

Decided 2026-11-25 (maintainer, the pick round on
[moq-dev/moq#9553](https://github.com/moq-dev/moq/pull/9553)):

- ✅ lite-07 carries the unsigned decode-order deltas, so the
  [lite-07 cut](/quest/m2/lite07-finalize.md) waits for this quest (rejected:
  the next wip version after the cut, with the `cts` encoder switch).
- ✅ A producer refuses a frame whose timestamp is below its predecessor's,
  where the frame is created (rejected: egress clamping it up to the previous
  timestamp, as the branch does, which silently rewrites B-frame presentation
  times and needs per-copy clamp state). First-party encoders therefore switch
  to `cts` in the same change, since a `legacy` B-frame stream's presentation
  timestamps go backwards.

- A `cts` track's Group Starts are decode-order times while HLS/DASH grid
  lines are presentation times (`rs/moq-hls/src/export/grid.rs`). When `cts`
  encoders land, decide which time segment placement uses on a `cts` track and
  state it in `drafts/draft-lcurley-moq-hang.md`.
- The Draft-18..22 shim that keeps legacy `TIMESCALE` (`0x08`) and dense
  Object IDs for mixed-version rollouts stays, marked for deletion at the next
  bump.
- Update `drafts/draft-lcurley-moq-timestamp.md`, `drafts/draft-lcurley-moq-ietf.md`,
  `drafts/draft-lcurley-moq-e2ee.md` (frame index recovery for partial sparse
  delivery), `drafts/draft-lcurley-moq-lite.md` (FRAME and GROUP_END), the
  code point table in `drafts/README.md`, `doc/concept/moq-lite.md`, and
  `doc/concept/standard.md` with the code, as the branch already does.
