# [M] Ship capture and playback

## Goal

A released `moq` binary can capture and play, and carries every video backend
its platform can load without system libraries. Both features work and
neither is reachable today: every distribution builds default features, and
`capture` and `play` are not among them.

## Plan

Nix (`cargoExtraArgs = "-p moq-cli"`), Docker, winget, and
`cargo install moq-cli` all build defaults, so `moq import capture` and
`moq play` are unreachable for everyone who does not build from source. That
is a packaging gap, not a documentation one: `doc/bin/cli.md` already
documents the `--features capture` build.

Decided 2026-10-26 (maintainer): defaults stay lean, and releases build
everything. A default skips any feature that is expensive to build or relies
on a system library, so `cargo build` and `cargo install moq-cli` stay fast
and portable. moq-cli gains a `release` feature that turns on `capture`,
`play`, `openh264`, `vaapi`, `nvidia`, and `v4l2` where the platform has it,
and every distribution (the release binaries, Nix, Docker, winget, Homebrew)
builds with it. `doc/bin/cli.md` tells source builders to pass
`--features release`. This replaces the earlier plan to turn `capture` and
`play` on by default. Check moq-cli's current defaults (`cluster-lan`, `quic`,
`websocket`, `nvidia`, `s3`, `gcs`, `azure`) against the same rule and move
any that fail it into `release`.

The individual sub-features stay, so a self-compiler can still pick a subset.
`vaapi` and `nvidia` load their driver libraries at runtime (dlopen), so they
belong in `release`. The microphone pulls ALSA through cpal, which is why
`release` waits on [ALSA without a load-time link](/quest/m2/capture-alsa-link.md);
`play` pulls winit plus a GPU stack. macOS and Windows use OS frameworks and
add nothing.

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

`pipewire` stays out of `release` (decided 2026-10-06, reaffirmed
2026-10-26): it needs libpipewire-0.3 to load, the same load-time requirement
[ALSA](/quest/m2/capture-alsa-link.md) removes. It joins once it loads lazily.

Decided in the 2026-09-30 audit: the v4l2 encode quest folded in here, since
its remaining work was one release feature flag and a doc note.

Decided in the 2026-10-06 audit: this does not wait on the library
`capture` defaults. moq-cli's `capture` feature enables `moq-video/capture`
and `moq-audio/capture` explicitly, and `release` enables `capture`.

## Required

- [Audio capture without runtime system libraries](/quest/m2/capture-alsa-link.md) - the microphone path must start without system audio libraries before every distribution can ship it
