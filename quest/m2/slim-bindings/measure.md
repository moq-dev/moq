# [XS] Measure the codec share on iOS and Android after LTO

## Goal

A table of stripped moq-ffi library sizes on `aarch64-apple-ios`,
`aarch64-linux-android`, and `armv7-linux-androideabi`, default versus
`--no-default-features`, built with the release profile after link-time
optimization landed. The table is the go or no-go for the rest of the line:
under roughly 15% saved on both platforms, the questline is abandoned in this
quest's PR and the decision is recorded in the PR description; above it, the
three packaging quests proceed.

## Plan

Run the nightly `size` recipe from the release size quest with `--target` for
the three mobile targets (the Android NDK and iOS SDK are on the release-ffi
runners; locally the Nix shell carries cargo-ndk). Add the mobile targets to
the nightly size report while there so the number stays visible. Record the
per-crate `cargo bloat` output for the default build too: on Android the cost
is openh264's C objects and libopus, on iOS it is libopus alone.

No source change beyond the recipe's target list. The outcome updates
[the questline](/quest/m2/slim-bindings/README.md): either the three quests
lose this `Required` entry, or the directory is deleted.

## Required

- [Release size](/quest/m2/release-size.md) - the profile change moves every number, so measuring before it lands is wasted
