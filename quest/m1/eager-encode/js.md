# [M] @moq/publish encodes while enabled

## Goal

`@moq/publish` audio and video encoders run whenever their rendition is
enabled, with or without a subscriber. Their groups sit in the track's cache
for a late SUBSCRIBE or FETCH to reach within its `maxDelay`. An `onDemand`
option restores today's gate.

## Plan

Decided 2026-10-28 (maintainer); see the [epic](/quest/m1/eager-encode/README.md).

- The root gate: `js/publish/src/broadcast.ts:413-462` sets `rendition.track`
  only while `served.demand().wanted` is defined. Both encoders key off it
  (`video/encoder.ts:341-349`, `audio/encoder.ts:401-416` and `:686-688`).
- Losing demand no longer ends an epoch (`#cut()` on demand loss). Disabling
  still does.
- Video still answers a join that wants a newer group with a fresh keyframe
  (`demand.wanted`).
- `onDemand` is one option on the broadcast, applying to every rendition.
  `doc/lib/js/publish.md:146` describes both modes.

Tests: an enabled encoder with no subscriber writes groups. A subscriber that
arrives later with a covering `maxDelay` receives the earlier groups.
`onDemand` keeps today's behavior.

Public API: default behavior change, plus an additive `onDemand` option.
Wire: none.
