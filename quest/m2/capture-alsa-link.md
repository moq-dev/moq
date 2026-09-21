# [S] Audio capture without link-time ALSA

## Goal

moq-audio `capture` and `playback` build on Linux without linking libasound,
so the microphone path stops blocking capture-by-default.

## Plan

Narrow the cpal backend set so a Linux capture build does not link libasound
unless the ALSA backend is asked for. Keep the `pipewire` and `pulseaudio`
host flags independent and never activating cpal alone, aligning with
[Optional native compilation](/quest/m0/media-features.md), and document the
capability combination.

Verify by building the capture and playback features on a host without the
ALSA development libraries and by inspecting the dependency graph to prove
libasound is absent when unasked. The PR 3850 capture gate keeps the coverage.

## Related

- [Ship capture and playback](/quest/m2/cli-packaging.md) - shippable capture needs this first
- [Optional native compilation and opt-in rendering](/quest/m0/media-features.md) - the host-flag rule this aligns with
