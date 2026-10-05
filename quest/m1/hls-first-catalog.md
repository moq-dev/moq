# [XS] moq-hls import publishes every rendition in its first catalog

## Goal

moq-hls import holds its first catalog until every rendition selected from
the master playlist has reserved its tracks, so a consumer never sees a first
catalog with only the first rendition.

Today each rendition's importer, and its `catalog.reserve()`, is created
lazily in `TrackState::ensure_map` (`rs/moq-hls/src/import.rs`) while `step`
ingests renditions one at a time. The first rendition's reservation can
release before the others exist, so the catalog publishes with one rendition
and grows on later updates. Reported by Dryvnt in
[#4824](https://github.com/moq-dev/moq/pull/4824).

## Plan

Decided (2026-10-05, maintainer): m1, [XS], ranked right after [Import at the
first frame](/quest/m1/import-first-frame.md). That quest moves each
importer's release to its first frame, which still lands before the next
rendition is reserved.

Guidance:

- `step` already holds a pass-wide timeline reservation for the same reason
  ("a record flushed mid-pass would omit every rendition that hasn't loaded
  its init segment yet"). A catalog `Reserved` held the same way, at least
  across the first pass, is the likely shape.
- Don't let a hold outlive its pass. A rendition that fails at startup under
  `OnError::Warn` must not withhold the catalog for the whole import, and a
  live `Reserved` withholds it (see [Shared import
  clock](/quest/m1/shared-clock.md), which rejected sharing offsets through
  one for that reason).
- Regression test (fails on `main`): a master playlist with two or more
  renditions, for example a video variant and a separate audio rendition
  from `file://` fixtures as the existing import tests use. The catalog
  consumer's first snapshot lists every rendition.

Public API: none. Wire: none. A multi-rendition HLS import's first catalog
lists every rendition instead of only the first.

## Related

- [Import at the first frame](/quest/m1/import-first-frame.md) - moves each importer's catalog release to its first frame
- [Shared import clock](/quest/m1/shared-clock.md) - moq-hls renditions share one `catalog::Input`, whose `reserve()` this hold may use
