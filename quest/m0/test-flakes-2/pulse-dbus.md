# [S] Test PulseAudio servers stay off the desktop's D-Bus

## Goal

Every private PulseAudio server the test suites start stays off the session
bus, so two media runs on one desktop never collide on the
`org.PulseAudio1` name. Concurrent `just test media` runs on a desktop host
pass where today one can fail with `D-Bus name org.PulseAudio1 already taken`.

## Plan

Planned 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run), from
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716). Milestone: ✅ m0.

`test/lib/pulse.py` already drops `DBUS_SESSION_BUS_ADDRESS` from its own
daemon's environment, so another launch path still reaches the bus: find it
(the Playwright media runner, `test/native-play`, `test/audio-quality`, or a
module PulseAudio loads by default) and close it at the source, for example by
starting the daemon without its D-Bus modules. Reproduce with two overlapping
media runs first.

Public API: none. Wire: none.
