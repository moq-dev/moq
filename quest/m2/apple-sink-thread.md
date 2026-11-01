# [M] moq-video's Apple Sink encodes on its own thread

## Goal

On macOS and iOS, a `moq_video::encode::Sink` resizes and encodes on its own thread, as on other platforms, so a forced-CPU or software-decoded transcode ladder never blocks a tokio worker.

## Plan

Follow-up of [moq-dev/moq#7375](https://github.com/moq-dev/moq/pull/7375), which moved rung resizing into the encoder's thread everywhere except Apple, where `Sink` runs inline. The comment in `sink.rs` saying `CVPixelBuffer` is `!Send` is stale: `PixelBuffer` is `Send`.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Give Apple the same threaded Sink as the other backends and delete the inline path and the stale comment. Needs a macOS host to verify: run the moq-video encoder tests and a transcode ladder with a forced CPU path, and record that the tokio worker stays free.

Public API: none. Wire: none.

## Required

- [Transcode rungs](/quest/m2/transcode-rungs.md) - its PR, #7375, adds the encoder-side resize
