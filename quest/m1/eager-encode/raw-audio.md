# [M] Audio encodes on demand from a raw backbuffer

## Goal

With no subscriber, the audio publishers in JS and Rust keep captured raw
audio up to the track's `maxAge` instead of encoding it. When demand arrives,
they encode that backlog and continue live, so a late subscriber can still
fetch the recent past within its `maxDelay`. This becomes audio's default.
Video stays always-on.

## Plan

Decided 2026-10-28 (maintainer): audio is cheap to keep raw (48 kHz stereo
f32 is ~384 KB per second retained), unlike video. So audio gets back the CPU
on-demand saved, without losing the past.

- The capture keeps running, so the microphone stays open as under always-on.
- The backlog encodes as live would have: the same frame size, the same
  timestamps, one group per frame as today. Encoded groups land in the cache
  in order, ahead of the live edge.
- A track with no `maxAge` keeps a fixed cap, chosen and justified in the PR.
- Leaving demand stops encoding and resumes buffering. The epoch continues, so
  the past stays addressable.
- JS (`js/publish/src/audio/encoder.ts`) and Rust (`rs/moq-audio` capture)
  both switch. The always-on default from their sibling quests becomes the
  opt-out.

Tests: no subscriber for a few seconds, then a subscriber with `maxDelay`
covering the last second receives that second, timestamps matching capture
time. Memory stays bounded by `maxAge` or the cap.

Public API: default behavior change for audio. Wire: none.

## Required

- [@moq/publish encodes while enabled](/quest/m1/eager-encode/js.md) - the JS always-on path this switches from
