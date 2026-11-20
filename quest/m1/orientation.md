# [M] Players pick a rendition by the viewport's orientation

## Goal

When a broadcast's video renditions mix portrait and landscape, `@moq/watch`
and `moq play` prefer the renditions shaped like their viewport, and switch
when it rotates or is resized past square. A broadcast whose renditions share
one orientation selects as today. A consumer with no viewport, such as
moq-mux's export, a recorder, or a restream, takes the catalog's ranking
(`VideoConfig::preference`, then hang's `ranked` order).

## Plan

Decided 2026-11-20 (maintainer, /quest-plan):

- ✅ portrait and landscape are renditions of one broadcast, told apart by
  their coded size (rejected: a catalog field naming a sibling portrait
  broadcast; a `<broadcast>/portrait` path convention in every player).
- ✅ with no viewport, the catalog's ranking decides, so a publisher whose
  original is portrait ranks portrait first (rejected: always landscape; an
  error when orientations mix and none is asked for).

Orientation filters before the existing pixel, dimension, and bitrate
selection (`js/watch/src/video/source.ts`), so a phone in portrait takes the
best portrait rendition that fits its rate. A square rendition matches either.

Found by [moq-dev/studio#200](https://github.com/moq-dev/studio/pull/200),
which publishes a portrait program beside the landscape one; the studio moves
it into the program's renditions once this lands.
