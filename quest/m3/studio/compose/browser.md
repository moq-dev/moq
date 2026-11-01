# [L] Browser composition

## Goal

`@moq/watch` composes a broadcast that carries a layout track: it fetches the
tracks the layout shows and draws them through moq-scene's browser build,
matching the studio's picture. A broadcast without one plays its program as
today, so embedders change nothing.

## Plan

The browser build comes from whichever line
[JS-only @moq/net from Rust](/quest/m2/rs2ts-js.md) lands (decided 2026-10-26; moq's WASM
support was removed on 2026-10-31), so this quest does not fix the
mechanism. A thin package (e.g. `@moq/scene`) wraps it and `@moq/watch` uses
it. Frames reach it as WebCodecs `VideoFrame`s through the same browser import
path the [web studio](/quest/m3/studio/web/video-web.md) adds.

Prove parity with a fixture: the same broadcast composited here and by the
studio, compared within a stated tolerance.

## Required

- [Layout track](/quest/m3/studio/compose/layout.md) - what the browser composes
- [moq-video in the browser](/quest/m3/studio/web/video-web.md) - browser frames into wgpu
