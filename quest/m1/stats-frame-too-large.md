# [XS] Stats tracks fail loud on an oversized frame

## Goal

A stats track whose next frame exceeds the cache limit ends with
`FrameTooLarge` instead of silently serving its previous snapshot, so a
reader learns the track stopped tracking the relay.

## Plan

- `TrackPair::publish` in `rs/moq-stats/src/produce.rs` gets
  `FrameTooLarge` from `update` (payload over `group::MAX_CACHE_BYTES`) and
  only logs it at debug level; readers keep the stale frame indefinitely.
  `publisher.json` near 50k paths is the likely first hit.
- Abort only the flavor whose `update` failed (log at warn) and keep its
  sibling publishing: DEFLATE can shrink an oversized plain frame or
  slightly grow a fitting one. Test that the overflowing flavor ends with
  `FrameTooLarge` while the other keeps updating. Prefix tracks get the same
  rule from [Stats prefix tracks](/quest/m2/stats-prefix-tracks.md).
- Found in #5719.
