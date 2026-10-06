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
sub-features already exist for this: `nvidia` is opt-out, `vaapi` and
`pipewire` are opt-in because they need libclang or libpipewire on the build
host, and `v4l2` is opt-in only by habit now that moq-v4l checks its bindings
in, so a self-compiler can shed CUDA or add the Linux extras, and
`--no-default-features` still reaches a minimal build. The cost is real and
lands on Linux source builds: the microphone pulls ALSA through cpal, and
`play` pulls winit plus a GPU stack. The camera path costs nothing. macOS and
Windows use OS frameworks and add nothing.

Check each distribution actually builds: the Nix overlay needs those system
dependencies present, and a Docker image without them fails at build rather
than at run. Then verify the shipped artifact runs `moq devices` and
`moq play` on each platform, since a feature that compiles into the binary and
then fails to open a device is the same gap one layer down.

Also enable `v4l2` in the Linux ARM release build, so a released binary on a
Raspberry Pi 4 publishes from `moq import capture` through the V4L2 M2M
hardware encoder (`rs/moq-video/src/v4l2.rs`, already run on a Pi 4's
`bcm2835-codec`) with no GStreamer detour. That Pi 4 run covered only
640x360 once, so the Pi 4 check also covers `set_bitrate` on a running
encoder (congestion control retunes through it) and 1080p, which codes as
1088 rows and relies on the compose rectangle to crop back.

Add a board hardware note to `doc/bin/cli.md` next to the capture build
instructions: Raspberry Pi 5 has no video encoder and Jetson Orin Nano ships
without NVENC; Pi 4, CM4, Zero 2 W, and Orin NX and above encode. RK3588
encodes through rkmpp in a vendor kernel, not V4L2, so it stays on the
`moq-gst` route.

`pipewire` stays off in shipped builds (decided 2026-10-06): it needs
libpipewire-0.3 to load, the same load-time requirement
[ALSA](/quest/m1/capture-alsa-link.md) removes.

Decided in the 2026-09-30 audit: the v4l2 encode quest folded in here, since
its remaining work was one release feature flag and a doc note.

## Required

- [Capture by default](/quest/m1/capture-default.md) - also changes moq-cli's `capture` feature; land it first
- [Audio capture without runtime system libraries](/quest/m1/capture-alsa-link.md) - the microphone path must start without system audio libraries before every distribution can ship it
