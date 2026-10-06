# [S] Media audio-tone check holds under load

## Goal

The `just test media` audio-tone check passes under load, fixed at its cause.
It failed with 66 of 75 samples audible in #4719's runs and 72 of 81 in
the 2026-10-06 late-join loops (#4914), and passed on rerun.

## Plan

First decide whether the missing samples are real playback (a stall,
underrun, or late start the check correctly catches) or how the check samples
the tone. Reproduce it looped under synthetic CPU load (approved by the
maintainer for these runs) before changing anything, and don't widen the
window or lower the agreement threshold to hide the symptom. Keep this
standalone because its root cause is independent of the late-join regression;
apply the load-flake questline's cause-first rules and include it in the final
loaded check.

## Related

- [More tests under load](/quest/m1/test-flakes-2/README.md) - shares the loaded-check validation
- [Media late join](/quest/m1/test-flakes-2/media-late-join.md) - shares the media fixture, but tracks a separate failure
