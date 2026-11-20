# [M] The last consumers that number groups use their start

## Goal

Every first-party consumer works by group start, in Rust and JS: the JS
container consumer judges continuity without `start + 1` adjacency, the
HLS/DASH export places each group on the join interval grid by its start
rather than its first frame's pts, and no doc or parameter still calls a
start a sequence. hang keeps writing and reading its duration and
discontinuity markers.

## Plan

Decided 2026-11-19 (maintainer, on closed
[moq-dev/moq#9192](https://github.com/moq-dev/moq/pull/9192)): ✅ hang keeps
its in-band group end markers (empty duration frame, discontinuity marker
group) on every transport (rejected: readers infer stops from lite-07's
GROUP_END position). This reverses the 2026-11-16 decision that dropped the
markers before the cut, and GROUP_END's timestamp goes instead (see
[group-end-whole](/quest/m2/time-addressing/group-end-whole.md)). So
`Read::End`, `Producer::discontinuity`, `Container::end`, and
`Container::finish_group` keep their meaning, and the hang draft keeps its
marker rules.

Still valid from the old consumers quest: convert each consumer that numbers
groups, update the hang draft and the docs each one makes stale, and rename
no CLI flag. [The model](https://github.com/moq-dev/moq/pull/8638) already
converted the Rust consumers whose API named group numbers. Checked on main
2026-11-19:

- Already by start: the Rust container consumer, moq-mux `seek`, HLS import,
  transcode (`moq-transcode/src/feed.rs:208`), archive, relay `/fetch?group=`
  and `moq fetch --group`, and `js/watch` (order comparisons only).
- Still adjacency by sequence: the JS container consumer
  (`js/hang/src/container/consumer.ts:192` `continues`, `:809` `adjacent` in
  `#checkMaxDelay`, `:1349` buffered ranges, and the `seq + 1n` fallback at
  `:1157`). It already uses `isSkipped`/`nextInRange` at `:1056` and `:1069`;
  judge continuity the way the Rust consumer does (`pts_contiguous`,
  declared skips, `next_in_range`).
- Placement: the HLS/DASH export places groups on the grid by their first
  frame's pts (`rs/moq-hls/src/export/rendition.rs:2140-2159`,
  `export/grid.rs:282-300`); place them by group start without changing the
  segment numbers. It also reads discontinuity marker groups
  (`First::Marker`, `rendition.rs:2149-2152`), which stay.
- Stale names: transcode's `lib.rs:18` doc ("sequence numbers 1:1"), the TS
  import's `seek(sequence)` parameter (`ts/import.rs:1805`, `:2392`,
  `:2644`), archive's `sequence` field holding a start
  (`moq-archive/src/producer/track.rs:241`, rename only if it is not part of
  the stored format), and `js/watch` comments.

Tests: a JS reader over groups whose starts are not consecutive integers
reads them as continuous where the media is; an HLS export of a track whose
first frame sits above its group start lands in the same segment as before.

Public API: none expected; a renamed parameter is not a break. Wire: none.

## Related

- [GROUP_END is a bare whole marker](/quest/m2/time-addressing/group-end-whole.md) - the wire half of the same re-plan
