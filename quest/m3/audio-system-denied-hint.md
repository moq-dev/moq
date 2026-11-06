# [XS] Denied macOS system audio names its settings pane

## Goal

A macOS system-audio source denied the Screen & System Audio Recording
permission fails with a permission error naming that System Settings pane, as
the display source and microphone already do, instead of the OS's "The user
declined TCCs".

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

moq-audio's system audio surfaces `NSError.localizedDescription` as a capture
error (`rs/moq-audio/src/capture/screencapture.rs`). moq-video's display
source checks first and names the pane (`ensure_screen_access` in
`rs/moq-video/src/capture/screencapture.rs`), and the microphone path does too
(`rs/moq-audio/src/capture/permission.rs`). Reuse that check rather than
matching on the OS error.

Consumer: the studio's system-audio source
([moq-dev/studio#14](https://github.com/moq-dev/studio/pull/14)).
