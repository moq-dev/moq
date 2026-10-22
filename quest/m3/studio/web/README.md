# Web studio

## Goal

The full studio runs in a browser from a `/studio` page in demo/web:
- the same egui UI and moq-scene compositor on WebGPU
- capture through getUserMedia and getDisplayMedia
- encode and decode through WebCodecs
- publishing over WebTransport through moq-net

Browser sources are absent, and so is anything else a browser cannot reach.
Each missing feature is hidden, not shown broken.

This README's own work is the studio's wasm build and that page. The children
add the browser media backends it needs.

## Plan

Decided 2026-10-22: a full wasm studio, not a remote control for a native
one. Media goes through browser backends in moq-video and moq-audio, not a
bridge to js/publish and js/watch, so frames never cross into a second media
stack.

## Required

- [The studio app](/quest/m3/studio/app.md) - the app being built to wasm
- [hang and moq-mux on wasm](/quest/m3/studio/web/wasm-mux.md) - the media crates build for wasm32
- [moq-video in the browser](/quest/m3/studio/web/video-web.md) - browser capture, WebCodecs encode and decode, and VideoFrame import into wgpu
- [moq-audio in the browser](/quest/m3/studio/web/audio-web.md) - AudioWorklet capture and playback with WebCodecs Opus and AAC
