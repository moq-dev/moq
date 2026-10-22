# [M] Bound the caption cues and regions a peer can accumulate

## Goal

A caption track from an untrusted publisher cannot grow a viewer's cue store, or its per-frame work, faster than the viewer plays:

- `store.cues` and `store.regions` in `js/watch/src/text/renderer.ts` are bounded by the playhead window and a per-track ceiling.
- A payload with many cues parses in O(n log n), not by walking and splicing per cue.
- A cue's declared end time cannot keep it resident past a maximum duration.
- A region no retained cue references is dropped.

Today `pruneCues` drops only cues whose end is behind the playhead, so a far-future cue is never pruned, and `store.regions` is never pruned at all. `insertCue` walks backwards and splices at the index for every cue, so a descending payload is quadratic. `commit()` rebuilds the overlay from the whole store on every group, so the per-frame cost compounds the growth.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Clamp a cue's end to its start plus a maximum cue duration, and drop cues behind the existing `PRUNE_BEHIND` horizon.
- Prune regions that no retained cue references in the same sweep, and cap both maps, evicting oldest first. Neither cap refuses a payload; they bound what the viewer retains.
- Insert by binary search, or sort once per payload, instead of the backward walk plus splice.
- Keep the double-escaped `sanitizeCue` and its renderer-version test unchanged: the audit confirmed the pinned `media-captions` builds only double-quoted attributes, so the current escape set is sufficient.

Tests cover a future-dated cue stream, a fresh region per frame, and a descending 100k-cue payload, each asserting the bounded store and the same rendered text.

Public API: none.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the other viewer-side measurement the audit bounded
