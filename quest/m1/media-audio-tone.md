# [S] Media audio-tone check holds under load

## Goal

The `just test media` audio-tone check passes under load, fixed at its cause.
It has failed with 72 of 81 samples audible, in #4719's runs and again in the
2026-10-06 late-join loops, and passed on rerun.

## Plan

First decide whether the missing samples are real playback (a stall,
underrun, or late start the check correctly catches) or how the check samples
the tone. Reproduce it looped under synthetic CPU load (approved by the
maintainer for these runs) before changing anything, and don't widen the
window without a reason.
