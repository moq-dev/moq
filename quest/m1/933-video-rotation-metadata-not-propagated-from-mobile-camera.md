# [S] Video rotation metadata not propagated from mobile camera publish to watch renderer

## Goal

Implement and verify the behavior tracked in [#933](https://github.com/moq-dev/moq/issues/933)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: the watch renderer already applies
catalog rotation on dev and file import rotates stored footage. What remains
is the publish side: detect device orientation during live camera capture and
set catalog rotation. Land it on dev.

### Issue context

#### Summary

When publishing from a mobile device (e.g. iPhone camera via `<hang-publish>`), the video orientation is incorrect on the watch side. The phone captures in landscape natively (e.g. 640x480) even when held in portrait, but no `rotation` metadata is included in the catalog, and the watch-side renderer doesn't apply rotation even if it were present.

#### Steps to Reproduce

1. Open a `<hang-publish>` page on an iPhone (using WebSocket fallback via `@moq/web-transport-ws`)
2. Select camera source, hold phone in portrait orientation
3. Open the corresponding `<hang-watch>` page in another browser
4. Video appears rotated 90 degrees  -  the viewer sees the image sideways

#### Observed Behavior

- Catalog contains `640x480` coded dimensions with no `rotation` field
- The watch-side canvas renderer (`watch/video/renderer.js`) handles `flip` but not `rotation`
- The publish side (`publish/video/index.d.ts`) exposes `flip` as a Signal but has no `rotation` Signal

#### Expected Behavior

- The publish side should detect device orientation (e.g. via `VideoFrame.rotation`, `window.screen.orientation`, or MediaStreamTrack settings) and include `rotation` in the catalog
- The watch-side renderer should read `catalog.rotation` and apply the appropriate transform when drawing frames to canvas

#### Analysis

The catalog schema already supports `rotation` (it appears in the type definitions for both publish and watch catalog types). The gap is:

1. **Publish**: `Video.Root` has a `flip: Signal<boolean>` but no corresponding `rotation` signal, so rotation is never set in the catalog
2. **Watch**: `renderer.js` lines 100-105 check `catalog.flip` and apply `ctx.scale(-1, 1)` but have no corresponding rotation logic

#### Environment

- `@moq/hang` v0.1.2
- `@moq/web-transport-ws` (WebSocket fallback)
- Publishing from iPhone Safari (portrait), watching in Chrome desktop
- Relay: `cdn.moq.dev`

#### Workaround

For now we're applying a CSS rotation on the watch side based on aspect ratio heuristics, but this is fragile and doesn't handle all cases correctly.

## Closes

- [#933](https://github.com/moq-dev/moq/issues/933) - close this issue when the quest finishes
