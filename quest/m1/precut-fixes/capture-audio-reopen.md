# [S] moq import capture keeps publishing when its microphone fails

## Goal

`moq import capture` survives a microphone that fails mid-session (a USB mic
re-enumerating, another app taking the default input, a sample-rate change):
the audio track reopens the device with backoff, as `release` did, and the
video track never stops because audio failed.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; confirmed from source. In
`rs/moq-cli/src/publish.rs` the audio future returns the capture error and
`tokio::try_join!` fails the whole command, video included.
[moq-dev/moq#7706](https://github.com/moq-dev/moq/pull/7706) removed the
library's automatic reopen on purpose (`rs/moq-audio/MIGRATE.md`: "no
automatic device reopen"), but nothing moved the reopen into the CLI.
`release` recovered within 0.5 to 4 s.

Decided 2026-11-17 (maintainer, [moq-dev/moq#9010](https://github.com/moq-dev/moq/pull/9010#issuecomment-6959731587)): ✅ the CLI reopens the capture and its publication in a loop
with `release`'s backoff, keeping #7706's decision that the library does not
(rejected: putting the reopen back in moq-audio, which reverses that
decision; ending only the audio track with no reopen). A failure that persists past the backoff ends only the audio
track, with its error, and the video keeps running.

Public API: none (CLI behavior). Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#7706](https://github.com/moq-dev/moq/pull/7706) - removed moq-audio's device reopen without moving it to the CLI
