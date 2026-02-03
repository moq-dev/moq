---
title: "@moq/hang-ui"
description: Ready-made UI components for MoQ playback and publishing
---

# @moq/hang-ui
A library of Web Components for MoQ media playback and publishing.
Drop them into your HTML and get a full-featured player or publisher with zero JavaScript required.

## Installation
```bash
bun add @moq/hang-ui
# or
npm add @moq/hang-ui
```

TODO: Jsdelivr CDN

## \<hang-watch-ui\>

A video player with controls for watching MoQ streams.

```html
<hang-watch-ui>
  <hang-watch url="https://cdn.moq.dev/anon" path="demo/stream" muted>
    <canvas style="width: 100%; height: auto;"></canvas>
  </hang-watch>
</hang-watch-ui>
```

**Included controls:**
- Play/pause button
- Volume slider
- Latency slider
- Quality selector
- Fullscreen button
- Buffering indicator
- Stats panel

## \<hang-publish-ui\>

A publishing interface with source selection and controls.

```html
<hang-publish-ui>
  <hang-publish url="https://cdn.moq.dev/anon" path="demo/stream">
    <video style="width: 100%; height: auto;" muted autoplay></video>
  </hang-publish>
</hang-publish-ui>
```

**Included controls:**
- Source selector (camera, screen, microphone, file)
- Camera picker (if multiple cameras available)
- Microphone picker (if multiple mics available)
- Publishing status indicator
- Stats panel

## Full Control
If you want full control over the interface, use the underlying `<hang-watch>` and `<hang-publish>` elements from `@moq/hang` directly.
 The `-ui` components just add the control overlay.

```html
<!-- Just the player, no controls -->
<hang-watch url="https://cdn.moq.dev/anon" path="demo/stream">
  <canvas></canvas>
</hang-watch>
```
