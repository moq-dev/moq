# [L] Decode time comes from decode order and fixed constants

## Goal

An exporter that needs a decode time (TS, MKV, FLV) derives it only from
the frames of the current group, in the decode order they arrive, and from
constants fixed for the rendition's life. Two exporters that read the same
group author the same DTS for it, whatever they joined at or lost before.
Media frames stay presentation-time only: no decode time on the wire.

MKV and TS export cannot retain unlimited frames or spend quadratic time
draining them when timestamps stop advancing: all exporter-owned reorder
storage is bounded by exporter capacity, with valid B-frame timing kept.

## Plan

Open (2026-10-22, maintainer): the counted approach below is in question
("counts instead of timestamps"). Re-plan it with `/quest-plan` before
starting; the decisions below stand until then.

Folded in 2026-10-22 (maintainer): the release audit's export reorder
storage quest. `DecodeClock::pop` (`rs/moq-mux/src/container/decode.rs`)
holds every frame in `held` and `slots` until the highest PTS passes the
lookahead, so flat timestamps retain unbounded bytes: 100,000 1 KiB frames
at one PTS released nothing until an explicit flush, and the flush scans
slots and removes element zero each time. MKV export inserts into the clock
before its jitter-queue backpressure (`mkv/export.rs`), and TS has the same
open-group exposure for timestamp-driven clocks. MKV also uses `pop` and
moves with TS. Add exporter-level open-group tests with flat and backward
PTS and valid reordering, and sweep buffered items and payload sizes for
retained memory and drain cost. It was introduced since release by
[#5309](https://github.com/moq-dev/moq/pull/5309), so it gates the cut.

Decided 2026-10-22 by the maintainer, after the TS root-cause audit:

- Frames carry PTS only, in decode order. A decoder needs order; a TS
  output needs *a* DTS that is monotonic, never after its PTS, and fits the
  T-STD buffer, not the source's own. Source DTS spacing (field coding,
  pulldown) is normalized, not preserved.
- Rejected: carrying decode time in the frame (CMAF composition offsets, a
  LOC property, Legacy inference). It adds a wire field to every container
  to preserve spacing no consumer needs.
- `DecodeClock::pop_counted` (`rs/moq-mux/src/container/decode.rs`), today
  the `--sync` path, becomes the only path. Delete the history-dependent
  `pop` settling and the delay that only grows; the catalog `jitter` stops
  feeding DTS.
- The constants: an optional per-rendition reorder delay in the Hang
  catalog, set by an importer that knows it (TS from source `PTS - DTS`,
  fMP4 from `ctts`/`trun` offsets, an encoder from its config) and fixed for
  the rendition's life. A source that exceeds it publishes a new rendition
  under a fresh name, as a PMT revision does. Absent, export falls back to
  the SPS: its declared reorder depth, else the level's `MaxDpbFrames`
  bound. Propose the field's name with alternatives in the PR.

Why: [moq-dev/moq#1843](https://github.com/moq-dev/moq/pull/1843),
[#2967](https://github.com/moq-dev/moq/pull/2967),
[#4500](https://github.com/moq-dev/moq/pull/4500),
[#4574](https://github.com/moq-dev/moq/pull/4574),
[#4645](https://github.com/moq-dev/moq/pull/4645) and
[#5949](https://github.com/moq-dev/moq/pull/5949) each patched a DTS that
depended on what an exporter had read before.

Rust: `rs/hang` catalog, `rs/moq-mux` TS, fMP4 import and TS, FLV export.
JS: `js/hang` catalog. Draft: `drafts/draft-lcurley-moq-hang.md` (the
catalog field), `drafts/draft-lcurley-moq-mpegts.md` (how export authors
DTS). Public API: a new optional catalog field; the wire change is additive.

Tests: two exporters joining one B-frame source at different groups, and
one losing a group, author identical DTS for every group both read; a source
exceeding its declared delay publishes a new rendition; a source without the
field falls back to the SPS bound. Keep the T-STD controls passing.

## Related

- [Deterministic TS legs for H.265](/quest/m2/ts-hitless-h265.md) - the `--sync` group anchor this path shares
- [Access units across PES](/quest/m1/ts-reliability/pes-access-units.md) - TS import's other video timing gap
