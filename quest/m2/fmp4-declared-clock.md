# [S] fMP4 decode times come from the declared reorder depth

## Goal

The fMP4 exporter authors decode times from the rendition's declared reorder depth with `container::declared::DeclaredClock`, as the TS, FLV, and MKV exporters do, so sub-GOP fragments of variable-rate B-frame video decode at their group's presentation slots instead of at durations capped at the catalog cadence: a stall longer than the cadence shows as its real gap, and only each group's first depth frames are spaced one tick apart, as in the other exporters. `moq export fmp4`, HLS packaging, and `moq export mp4` share the change.

## Plan

Follows [moq-dev/moq#7491](https://github.com/moq-dev/moq/pull/7491), which keeps fMP4 decode times increasing in fragments shorter than a GOP by capping inferred durations at the catalog cadence for a rendition that may reorder. That cap is exact for constant-rate B-frame video, but an H.264 or H.265 rendition that doesn't declare `reorder: 0` and stalls longer than the cadence gets the cadence rather than the real gap. #7491 named this as the alternative it didn't take. It needs #7491 to land first; this replaces its cap.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. One decode-time rule across every exporter is simpler than a cadence cap in one of them.

- `DeclaredClock` (`rs/moq-mux/src/container/declared.rs`) holds depth frames per group before releasing them and decodes the first depth frames one tick apart up to the earliest slot, so a DTS can fall before the timeline's zero. Settle how fMP4 writes that: a version 1 `trun` with signed composition offsets, or an edit list, and what each costs HLS and `export mp4`'s rebuilt sample tables.
- A rendition that declares no depth keeps today's inference, including #7491's rule.
- Keep #7491's tests (`tfdt` never steps back across sub-GOP fragments, ffprobe reads every fragment) and add a variable-rate B-frame case with a stall longer than the cadence, whose decode times follow the presentation slots across the stall. `DeclaredClock` derives DTS from PTS and depth alone, so a gap that only the encoder's own decode times would show is out of reach and not a goal.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - its nightly ffprobe checks would catch a regression here
