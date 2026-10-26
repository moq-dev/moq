# [S] Per-app audio on macOS

## Goal

`moq_audio::capture` captures one application's audio on macOS, so a creator
can stream a game without their music. The Linux and Windows halves live in
their capture parity quests.

## Plan

Decided 2026-10-26 (maintainer). ScreenCaptureKit already delivers system
audio here (`Source::System`), and an `SCContentFilter` that includes only one
application's windows limits the audio to that app. Apps are listed from
`SCShareableContent`, so only a running app can be chosen. It needs the
Screen Recording permission, like system audio.

## Related

- [Linux capture parity](/quest/m2/capture-linux.md) - per-app audio through PipeWire nodes
- [Windows capture parity](/quest/m2/capture-windows.md) - per-app audio through WASAPI process loopback
