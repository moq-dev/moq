# [XS] Backport the H.265 display aspect fix to release

## Goal

The `release` line publishes a moq-mux and libmoq whose H.265 importer puts
the picture's display aspect in the catalog, not the VUI's sample aspect, so
a 1280x720 stream signalling square pixels plays at 16:9 in @moq/watch
instead of square. Check: the newest libmoq release contains the fix.

## Plan

✅ Decided 2026-10-23 (maintainer, importing #6536): backport rather than wait
for the next cut. Why: the cut is gated on broadcast epochs, and libmoq users
run `release`.

Cherry-pick [moq-dev/moq#5574](https://github.com/moq-dev/moq/pull/5574) onto
`release` following CONTRIBUTING's backport workflow, adapting to the release
line's `codec/video.rs` helpers and keeping its tests. Public API and wire are
unchanged.

## Closes

- [#6536](https://github.com/moq-dev/moq/issues/6536) - H.265 importer puts the sample aspect in the display aspect fields
