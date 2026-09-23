# [M] Bindings expose the Gop enum

## Goal

Every binding names the group structure the way the core does: the ffi record,
the libmoq C struct, and the Python, Swift, Kotlin, Dart, and Go wrappers take
a keyframe interval or a refresh cycle and nothing else, and `cut()` keeps its
meaning in both modes. This replaces the published `gop` integer in moq-ffi
and changes the libmoq C struct layout, so it targets `dev`.

## Plan

- `rs/moq-ffi/src/video.rs`: `MoqVideoEncoderOutput.gop: Option<u32>` becomes
  a `MoqVideoGop` enum record mirroring `Gop`, defaulting to keyframes at two
  seconds. `MoqVideoProducer::cut()` already has the right name.
- `rs/libmoq/src/video.rs`: `moq_video_encoder_output` gains a
  `moq_video_gop` discriminant beside `gop`, zero meaning keyframes, and
  `moq.h` is regenerated (build.rs does not do it on source-only changes).
  `cpp/obs/src` follows the header.
- Hand-written wrappers and docs per the cross-package table: `py/moq-rs`,
  `swift/`, `kt/`, `dart/moq`, `go/wrapper/moq`, and `doc/lib/{py,swift,kt,go,dart,c}`.
  Go gets no uniffi default, so its zero value must read as keyframe mode.
- Run `just test smoke --all` for the cross-language check.

## Required

- [Encode config](/quest/m2/intra-refresh/encode-config.md) - the core enum this mirrors
