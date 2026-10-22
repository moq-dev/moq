# Web studio

## Goal

The full studio runs in a browser from a `/studio` page in demo/web:
- the same egui UI and moq-scene compositor on WebGPU
- capture through getUserMedia and getDisplayMedia
- encode and decode through WebCodecs
- publishing over WebTransport through moq-net

Browser sources are absent. So are RTMP and WHIP destinations. A page cannot
open raw TCP, and moq-rtc's WHIP client binds native UDP sockets, so only MoQ
output remains. Each missing feature is hidden,
not shown broken.

The children land the page as vertical slices: video first, then audio. This
README's own work brings the page to parity with the native app for
everything a browser can reach.

## Plan

Decided 2026-10-22: a full wasm studio, not a remote control for a native
one. Media goes through browser backends in moq-video and moq-audio, not a
bridge to js/publish and js/watch, so frames never cross into a second media
stack. Each backend lands with the page slice that uses it, never ahead of
it.

## Required

- [moq-video in the browser](/quest/m3/studio/web/video-web.md) - the first `/studio` page, with browser video capture, WebCodecs, and VideoFrame import into wgpu
- [moq-audio in the browser](/quest/m3/studio/web/audio-web.md) - audio on the page through AudioWorklet and WebCodecs Opus and AAC
