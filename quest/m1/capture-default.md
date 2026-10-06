# [S] Capture is a default feature

## Goal

`capture` is on by default in moq-video and moq-audio, so the pre-merge
`just check` compiles and tests device capture like any other default code,
and the separate capture CI gate goes away. The flag stays so a minimal build
can opt out. Workspace consumers that set `default-features = false` (the
relay, the language bindings) keep building without it.

## Plan

Decided in planning (09-29), after capture-control (#4545) found that
pre-merge `just check` never runs moq-video's `capture` tests:

- **Default-on, keep the flag.** Removing the flag was rejected: consumers
  that don't want device stacks would pay for them.
- moq-video's `capture` already costs nothing to build on any platform. Its
  Cargo comment says it stays off only "until every distribution ships it", a
  packaging concern this reverses.
- moq-audio's `capture` links libasound when it builds on Linux, which is why
  it depends on [ALSA link](/quest/m1/capture-alsa-link.md). Once libasound
  loads at runtime, the default costs no system library.
- Delete the special capture gate (`rs capture`, `capture-test`, and the
  capture branch in `sh/rs/select.sh`) once default
  `just check` covers it. Keep the platform jobs (`just rs macos`,
  `just rs windows`).
- Update the Cargo feature comments and `doc/` wherever capture is described
  as opt-in. moq-cli's `capture` feature stays: the root manifest pins
  moq-video and moq-audio to `default-features = false` for every workspace
  consumer, and [Ship capture and playback](/quest/m1/cli-packaging.md) makes
  it default-on and droppable.

Verify: `just check` on a moq-video or moq-audio change runs the capture
tests, and a `default-features = false` consumer (for example
`cargo tree -p moq-ffi -e features`) pulls no capture dependency.

## Required

- [ALSA link](/quest/m1/capture-alsa-link.md) - moq-audio capture can't be on by default while it links libasound at build time
