# [S] Audio capture without link-time ALSA

## Goal

moq-audio `capture` and `playback` build on Linux without linking libasound,
so the microphone path stops blocking capture-by-default.

## Plan

Narrow the cpal backend set so a Linux capture build does not link libasound
unless the ALSA backend is asked for. The current matrix is the contract to
change: `capture` and `playback` pull cpal, whose `alsa` dependency is
non-optional on Linux in cpal 0.18, so libasound links whenever either is on;
`pipewire = ["cpal/pipewire"]` and `pulseaudio = ["cpal/pulseaudio"]` activate
cpal without either. The target matrix keeps those host flags but makes them
require `capture` or `playback` so they never activate cpal alone, and puts
the ALSA backend behind an opt-in capability instead of the always-linked
default. Whether that capability comes from a cpal release gating `alsa` or
from in-tree handling is the implementation decision; document the supported
combinations in the moq-audio feature table with the same change.

Verify by building the capture and playback features on a host without the
ALSA development libraries and by inspecting the dependency graph to prove
libasound is absent when unasked. The PR 3850 capture gate keeps the coverage.

Verify by building the capture and playback features on a host without the
ALSA development libraries and by inspecting the dependency graph to prove
libasound is absent when unasked. The PR 3850 capture gate keeps the coverage.

## Related

- [Ship capture and playback](/quest/m2/cli-packaging.md) - the shippable capture milestone this work supports
- [Optional native compilation and opt-in rendering](/quest/m0/media-features.md) - the host-flag rule this aligns with
