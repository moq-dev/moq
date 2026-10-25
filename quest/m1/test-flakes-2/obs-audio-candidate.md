# [S] The OBS failed-audio-candidate test passes under load

## Goal

The OBS test "failed audio candidate preserves playback" passes under load,
fixed at its cause.

## Plan

Seen by [moq-dev/moq#6711](https://github.com/moq-dev/moq/pull/6711): it fails
intermittently under load on the base commit too. Reproduce it under CPU
pressure or `SIGSTOP`/`SIGCONT` pauses, the way
[moq-dev/moq#6737](https://github.com/moq-dev/moq/pull/6737) did, then fix the
race.

Public API: none. Wire: none.
