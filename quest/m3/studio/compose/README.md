# Viewer-side composition

## Goal

A `moq-studio` broadcast carries each source as its own track plus a layout
track describing the live scene. Viewers compose the picture themselves:
they prioritize sources and toggle elements on and off. A composited program
remains for protocols and devices that cannot compose (RTMP and WHIP
destinations, HLS, legacy players), produced by the studio or by a server.

This README's own work is the end-to-end test: a studio broadcast is composed
by the browser, by `moq compose`, and by the studio itself, and all three
pictures match.

## Plan

Decided 2026-10-26 (maintainer), the studio's flagship and first in its epic:

- **Delivery:** MoQ's value is per-track delivery, so the studio embraces
  viewer composition. The program is the fallback, not the default.
- **One compositor, three hosts:** moq-scene renders in the studio, headless
  on a server (`moq compose` in moq-cli), and in the browser, so all three
  pictures come from one implementation.
- **Parity rule:** every scene feature (transitions, filters, effects,
  overlays) renders in all three hosts with the same layout, timing, and
  colors, within a tolerance. Each renderer keeps its own text antialiasing
  and blur, so the browser can draw overlays as real DOM. A feature that
  cannot meet this, such as a CEF browser source or an ML effect, is baked
  into its source's own track before publishing rather than living in the
  layout.
- **Wire layout:** local sources are tracks in the studio's one broadcast.
  Guests stay their own broadcasts, named through the hang catalog's
  `contributions` references.
- **Layout:** the live scene state (items, transforms, transitions, and each
  element's priority and toggleable flag) as a hang JSON track on the media
  clock, so any host composites the same frame the studio shows.
- **Browser build:** whichever line [JS-only @moq/net from Rust](/quest/m2/rs2ts-js.md)
  lands. moq's WASM support was removed on 2026-10-31.
- **Cost:** every source and the program follow MoQ's pull model and are
  encoded only while something subscribes, so a fallback nobody uses costs
  nothing.

## Required

- [Source tracks](/quest/m3/studio/compose/sources.md) - each source publishes as its own track in the studio broadcast, encoded only while subscribed
- [Layout track](/quest/m3/studio/compose/layout.md) - the live scene state as a hang JSON track on the media clock, specified in the hang draft
- [Browser composition](/quest/m3/studio/compose/browser.md) - `@moq/watch` composes a broadcast with a layout track through moq-scene's browser build
- [Server composition](/quest/m3/studio/compose/server.md) - `moq compose` renders the program from sources and layout, headless
- [Viewer controls](/quest/m3/studio/compose/controls.md) - viewers toggle elements and the layout's priorities steer what they fetch
