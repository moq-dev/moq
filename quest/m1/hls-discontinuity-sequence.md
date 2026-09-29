# [S] moq-hls segments carry the absolute discontinuity sequence

## Goal

Every `moq_hls::export::Segment` reports the discontinuity sequence it belongs
to, the value a recorder writes as `EXT-X-DISCONTINUITY-SEQUENCE`, so two
cursors on sibling renditions agree no matter when each was created. On `dev`,
`Segment::discontinuity` is a `u64` count of breaks since the cursor's
previous segment ([#4068](https://github.com/moq-dev/moq/pull/4068)). A cursor
created after its rendition rebinds starts its count from its own first row,
so it disagrees with a sibling on the baseline.

## Plan

Decided:

- Report the absolute sequence the timeline fanout already stamps on each
  row, instead of the difference between rows. A recorder writes
  `EXT-X-DISCONTINUITY-SEQUENCE` from the first segment and an
  `EXT-X-DISCONTINUITY` wherever the value changes.
- Land it on `dev` before the next moq-hls release, so it ships in the same
  breaking release as the existing `bool` to `u64` change rather than
  breaking the field twice.

Guidance:

- `Position` in `export/segments.rs` then only tracks `after`; the
  skip/emit baseline logic goes away. Check the serve path's playlist
  rendering (`rendition.rs`, `playlist.rs`) already derives its tags from the
  same stamp.
- The sequence is absolute within one `Broadcaster`. Document what a recorder
  should do when its broadcaster is rebuilt and the sequence restarts.
- Update the field doc and the tests that assert per-cursor counts; add one
  where a cursor created after a rebind reports the same sequence as a cursor
  that has run since the start.
- moq.pro's recorder and index store the count today; note the change for its
  pin bump.
