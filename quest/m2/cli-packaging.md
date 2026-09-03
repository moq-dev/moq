# [M] Ship capture and playback

## Goal

A released `moq` binary can capture and play. Both features work and neither
is reachable: every distribution builds default features, and `capture` and
`play` are not among them.

## Plan

Nix (`cargoExtraArgs = "-p moq-cli"`), Docker, winget, and
`cargo install moq-cli` all build defaults, so `moq import capture` and
`moq play` are unreachable for everyone who does not build from source. That
is a packaging gap, not a documentation one: `doc/bin/cli.md` already
documents the `--features capture` build.

Turn both on by default and keep the heavy parts individually droppable. The
sub-features already exist for this: `nvidia`, `vaapi`, and `pipewire` are
opt-out, so a self-compiler can shed CUDA, libva, and libpipewire, and
`--no-default-features` still reaches a minimal build. The cost is real and
lands on Linux source builds: the camera path pulls libclang and V4L2 headers
through bindgen, the microphone pulls ALSA through cpal, and `play` pulls
winit plus a GPU stack. macOS and Windows use OS frameworks and add nothing.

Check each distribution actually builds: the Nix overlay needs those system
dependencies present, and a Docker image without them fails at build rather
than at run. Then verify the shipped artifact runs `moq devices` and
`moq play` on each platform, since a feature that compiles into the binary and
then fails to open a device is the same gap one layer down.

## Closes

- [#2272](https://github.com/moq-dev/moq/issues/2272) - close this issue when the quest finishes
