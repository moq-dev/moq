# [S] macOS capture listing futures are Send

## Goal

`capture::{displays, windows, apps}` and the capture start on macOS return
`Send` futures, so a caller can spawn them on a multi-threaded runtime
instead of wrapping them in `spawn_blocking`.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`screencapture::shareable_content` (`rs/moq-video/src/capture/screencapture.rs`)
keeps a ScreenCaptureKit completion `RcBlock`, which is not `Send`, alive
across its `await`; `start_capture` does the same, and so does moq-audio's copy
(`rs/moq-audio/src/capture/screencapture.rs`), whose compile-time `Send` check
is disabled on macOS (`rs/moq-audio/src/capture.rs`). Drop the block before
the await in each and turn the check back on.

Consumer: the studio's macOS sources ([moq-dev/studio#14](https://github.com/moq-dev/studio/pull/14)) work
around it with `spawn_blocking`; the workaround goes once this lands.
