# [M] Bounded HLS playlists

## Goal

A fresh viewer joins a days-old broadcast, live or archived, and `moq-hls`
renders its playlists with work bounded by the advertised window, not by the
broadcast's age. The media playlist lists a sliding window whose
`EXT-X-MEDIA-SEQUENCE` every edge and every reload agree on. A span with no
media in one rendition keeps its slot as a duration-preserving `EXT-X-GAP`
without holding back later segments or sibling renditions. A video rendition
is not advertised until its window holds a segment that starts at a group
start (a sync point), so a stock player is never sent to video it cannot
start decoding.

This is the generic renderer. Managed edges (moq.pro) only wire it to their
projects, routes, and auth.

## Plan

Decided 2026-10-05, from moq.pro's
[bounded live playlist](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/archive/playable.md):
the windowed sequence, gap slots, and sync-point gating are renderer rules
every `moq-hls` user needs, so they live here and moq.pro keeps only the edge
wiring. Rejected: keeping them in moq.pro. Also decided 2026-10-05: this
quest lives at the m1 root on `main`, outside the archive line, since live
timelines need it first.

The offline archive HLS that `quest/m1/archive/hls.md` used to describe is
done on the archive line's branch (#4115, #4155, #4169). The per-track gap
behaviour lives on that line's track-timeline questline
(`quest/m1/archive/track-timeline/README.md`, #4280, `spans.rs`), which
derives segments at the edge from group timestamps and emits `EXT-X-GAP` for
a rendition with no group start in a span. Whichever lands second rebases
onto the other, and the rules here hold for both shapes.

Start by auditing `rs/moq-hls/src/export` against each rule; much exists
(`segments.rs` numbers aligned segments and carries gap rows, `playlist.rs`
writes `EXT-X-GAP`). Fix only what a test shows missing:

- **Bounded join.** Rendering for a new viewer reads at most the window's
  records from the timeline (`moq_json::window`; a new group repeats at most
  `CHECKPOINT_RECORDS = 256` recent records, in `moq-mux`'s `timeline.rs`),
  never the whole history, and never GETs or FETCHes media. A live publisher
  that never pops its timeline must not make a join cost grow with broadcast
  age. The window is bounded by duration, so very short segments still mean
  many records per window: decide while building whether a record cap also
  applies, and test dense input against it.
- **Capped window, decided 2026-10-05.** A durable (store-backed) timeline
  still advertises a capped sliding window: a live playlist lists a bounded
  window even when the store retains more. A full VOD or EVENT listing is a
  separate, explicit replay mode. This conflicts with #4155's durable listing
  on the archive line branch, which lists the whole durable timeline without
  the live window; reconcile it there when that branch next merges `main`.
- **Stable sequence.** `EXT-X-MEDIA-SEQUENCE` is the window's first segment
  number, derived from the timeline, so two edges and a reload after a pop
  agree.
- **Gaps.** A span with no media in one track becomes a gap slot in that
  rendition only; siblings keep listing, and a rendition switch after a gap
  lands on the next real segment. A failed rendition timeline is not a gap:
  per #4552 its playlist ends at its last covered segment.
- **Sync-point gating.** The multivariant playlist omits a video rendition
  until its window holds a segment starting at a group start that is a sync
  point. Phrase the check by group start and sync point, not by keyframe, so
  it stays right for intra-refresh streams and agrees with
  [Export sync flags](/quest/m2/intra-refresh/export-sync-flags.md). A
  catalog that arrives first may expose audio only, or answer unavailable.

Prove: a fresh join after simulated days of publishing reads a bounded number
of records (count them); a durable timeline holding more than the window
still lists only the window; catalog arrival before the first group start;
gaps in one track; switching after a gap; healthy siblings keep their
listing; two exporters over one timeline render identical sequence numbers.
Tests mock time.

moq.pro pins `release`, so it adopts this through the next release that
carries it.

Public API: none expected. Wire: none.

## Related

- [moq.pro: bounded live playlist](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/archive/playable.md) - the edge wiring that adopts this renderer
- [Archive](/quest/m1/archive/README.md) - a replayed archive inherits these rules
- [Export sync flags](/quest/m2/intra-refresh/export-sync-flags.md) - which group starts the HLS export marks as sync points
