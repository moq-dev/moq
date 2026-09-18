# [XS] Measure the codec share on iOS and Android after LTO

## Goal

A table of what an app actually ships on `aarch64-apple-ios`,
`aarch64-linux-android`, and `armv7-linux-androideabi`, default versus
`--no-default-features`, built with the release profile the release size
quest settles. Android embeds `libmoq_ffi.so` verbatim, so its row is the
stripped shared library. iOS links `libmoq_ffi.a` out of the xcframework and
dead-strips it, so the raw archive overstates the saving; its row is the
binary of a smoke app linked against each variant. The table is the go or no-go for the rest of the line:
the line proceeds only if every one of the three targets saves at least 15%
of the stripped size, compared on the raw byte counts; otherwise the
questline is abandoned in this quest's PR and the decision is recorded in the
PR description.

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

- [Release size](/quest/m2/release-size.md) - supplies the `size` recipe and the final profile, so measuring before it lands measures the wrong build
