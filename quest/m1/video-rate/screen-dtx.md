# [M] Screen capture repeats at 4 Hz while still

## Goal

A still screen encodes a repeat at least every 250 ms instead of at the full
frame rate, on PipeWire, WGC, ScreenCaptureKit, and the JS publisher's
display capture and still-image sources. Changes still encode at the
configured frame rate. The player shows a still screen as live, not stalled,
and a viewer joining mid-stillness starts promptly.

## Plan

Decided with the maintainer on 2026-11-15 (/quest-plan): ✅ the keepalive is
an encoded repeat (a skip P-frame of a few dozen bytes) at a 4 Hz floor
(rejected: a zero-length moq frame, which would add new meaning to the hang
format, its draft, and every decoder, export, and gateway).

Today:

- PipeWire (`rs/moq-video/src/capture/pipewire.rs`) and WGC
  (`rs/moq-video/src/capture/wgc.rs`) re-emit the last frame every frame
  interval: move that timer to 250 ms.
- ScreenCaptureKit (`rs/moq-video/src/capture/screencapture.rs`) drops idle
  buffers and re-emits nothing, so a still Mac screen sends no frames at all:
  add the 250 ms repeat.
- X11 grabs every interval with no damage tracking; it stays at full rate.
- `js/publish/src/source/file.ts` republishes a still image at 30 fps; drop it
  to the 4 Hz floor. Check whether Chrome's display capture track delivers
  frames only on change, and add the repeat if it can go silent.

Check that keyframes still come on the GoP interval, since a timer based on
frame count would stretch to eight times as long at 4 Hz, and that
`@moq/watch`'s latency and stall handling treats 250 ms gaps as live.
WGC's change is the same timer value; no Windows host is listed, so its
unit test and the Windows CI build cover it.

Public API: none. Wire: none.

## Hosts

- linux - PipeWire
- macos - ScreenCaptureKit
