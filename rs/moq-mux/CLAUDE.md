The transmuxer: file/stream containers (`container/`) and codec parsers (`codec/`) in and out of hang broadcasts. `Producer<C>`/`Consumer<C>` are generic over the `Container` trait; the catalog picks the implementation per track.

# Invariants

- The keyframe bit drives grouping. Only the first frame of a group is a keyframe; for audio, mark one frame per intended group, not every frame, or each frame opens its own QUIC stream. `cut` closes a group early and the next write must be a keyframe.
- Frame durations are backfilled from the next frame in decode order, never across a discontinuity; a backwards gap (B-frame reordering) stays unset. Frames that arrive with a duration keep it.
- hang frames carry a timestamp normalized to microseconds (`hang::container::TIMESCALE`). `moq_net::track::Info::default()` is milliseconds, so pin the track with `with_timescale` when creating it.
- Reject an unsupported codec or container with a typed `Error` rather than warn and drop.
- Catalog fields that decide decoder configuration (codec, dimensions, description) are effectively immutable for a rendition; bitrate and similar hints are maximums.

# Cross-language

`js/hang` mirrors the catalog and container; `js/watch` and `js/publish` are the browser equivalents of the codec paths. A catalog or container change lands in both languages and in `drafts/draft-lcurley-moq-hang.md` in the same PR.
