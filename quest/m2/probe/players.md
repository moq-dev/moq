# [S] Players set viewer targets from the catalog

## Goal

`@moq/watch` holds one viewer handle per watched broadcast, set to the
maximum bitrate of that broadcast's highest enabled video rendition in the
catalog, and follows the catalog as renditions are enabled or disabled. The
handle is dropped when the broadcast stops being watched. `moq play` and
`moqsrc` do the same if they adapt renditions to bandwidth, which neither does
today. An application can override or disable the target.

## Plan

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)): the player sets the target because it
already reads the catalog, which keeps moq-net catalog-agnostic, and only an
enabled rendition is one it could switch to. Targets are on by default.

- The handle comes from the [target](/quest/m2/probe/target.md) quest; this
  quest only decides its `max`.
- Settle what a rendition without a catalog bitrate contributes.
- A probe that reaches the target must make the rendition selectable.
  Selection discounts the estimate (0.8x in `js/watch/src/video/source.ts`)
  and audio shares the downlink, so either the handle covers both or the
  [up-switch](/quest/m2/probe/viewer-upswitch.md) stops discounting a probed
  estimate; settle which with that quest.

Tests with mocked catalogs: the target tracks the highest enabled rendition
through enable and disable flips, ignores disabled ones, drops when unwatched,
and yields to an application override.

## Required

- [Session probe target](/quest/m2/probe/target.md) - the per-broadcast viewer handle
