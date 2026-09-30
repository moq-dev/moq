# [S] Bindings expose refresh mode

## Goal

Every binding names the group structure the way the core does: the moq-ffi
record and the Python, Swift, Kotlin, Dart, and Go wrappers take a keyframe
interval or a refresh cycle and nothing else, and `cut()` keeps its meaning in
both modes.

## Plan

- [Codecs](/quest/m1/ffi-shape/codec.md) already replaces the `gop` integer
  with a `MoqVideoGop` enum mirroring the non-exhaustive `Gop`, so this adds
  the refresh variant beside `Keyframe` in `rs/moq-ffi/src/video.rs`, which
  is additive and lands on `main`. `MoqVideoProducer::cut()` already has the
  right name.
- moq-ffi only: the generated C and C++ bindings inherit the variant.
- Hand-written wrappers and docs per the cross-package table: `py/moq-rs`,
  `swift/`, `kt/`, `dart/moq`, `go/wrapper`, and `doc/lib/{py,swift,kt,go,dart}`.
- Run `just test interop --all` for the cross-language check.

## Required

- [Codecs](/quest/m1/ffi-shape/codec.md) - the `MoqVideoGop` enum this extends
