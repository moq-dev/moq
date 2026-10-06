# [S] Audio capture without runtime system libraries

## Goal

A released moq-audio capture/playback build carries no runtime system
requirement on Linux: Nix provides everything needed at dev time to compile,
and the shipped binary starts and degrades cleanly on hosts without those
libraries.

## Plan

Nix already covers the dev-time side (alsa-lib, alsa-plugins, and pipewire
are in the dev shell). What remains is the runtime linkage: cpal 0.18's
`alsa` dependency is non-optional on Linux, so libasound becomes a load-time
requirement and the binary refuses to start where it is absent. Follow the
vaapi/nvidia pattern and load the system library at runtime instead, falling
through to the next host when it is missing, so a build with the feature on
still links and starts driverless.

`capture` and `playback` pull cpal with ALSA always linked; the host flags
alone no longer activate it. cpal cannot load ALSA at runtime in-tree (no
release offers it, and the in-tree routes are hacks), so this waits on
[the cpal release](/quest/m1/cpal-alsa-runtime.md). Once it ships, bump cpal,
enable whatever it needs, and update the moq-audio and moq-cli feature
comments that call libasound a build-time link.

Verify by building in the Nix shell, then running the shipped binary on a
host without libasound: it starts, lists devices, and captures where a
backend exists. Inspect the binary to prove no load-time requirement on
libasound. The PR 3850 capture gate keeps the coverage.

## Required

- [cpal loads libasound at runtime](/quest/m1/cpal-alsa-runtime.md) - condition: a cpal release whose Linux build carries no load-time libasound requirement

## Related

- [Ship capture and playback](/quest/m1/cli-packaging.md) - the shippable capture milestone this work supports
