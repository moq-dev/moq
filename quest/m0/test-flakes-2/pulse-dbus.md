# [XS] Concurrent PulseAudio harness runs don't share a D-Bus name

## Goal

Two media harness runs on one host each start a private PulseAudio without
`D-Bus name org.PulseAudio1 already taken`, so a concurrent run never fails
before its browser launches.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716)): milestone m0,
a child of the test-flakes epic, since parallel agent runs hit it.

- `test/lib/pulse.py` (lines 44–53) isolates `XDG_RUNTIME_DIR`, `PULSE_*`,
  `ALSA_CONFIG_PATH`, and `DISPLAY`. It passes `DBUS_SESSION_BUS_ADDRESS`
  through, so every private daemon tries to claim the same session-bus name.
  Callers: `test/interop/interop.sh:2118`, `test/audio-quality/run.sh:349`,
  and native-play.
- Give each daemon no session bus. Unset the address or disable PulseAudio's
  D-Bus module in the private config, after checking that the daemon starts
  cleanly with no bus. If it doesn't, run it under its own
  `dbus-run-session`.
- Add a test that starts two `pulse.py` instances at once and plays through
  each.

Public API: none. Wire: none.
