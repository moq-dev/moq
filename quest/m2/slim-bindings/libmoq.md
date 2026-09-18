# [S] libmoq: audio and video features and a libmoq-net release asset

## Goal

`rs/libmoq` gates `moq-audio` and `moq-video` behind `audio` and `video`
features, on by default, mirroring `rs/moq-ffi`. `--no-default-features`
yields a `libmoq.a` whose `moq.h` has no `moq_publish_video_raw`,
`moq_consume_video_raw`, or audio symbols. The libmoq release carries a
`libmoq-net-<version>-<target>` asset per target beside the full one. OBS keeps
building the full library.

## Plan

`src/audio.rs` and `src/video.rs` go behind `cfg(feature)`, and the entry
points in `api.rs` that reach them move into those modules or behind the same
cfg. cbindgen learns the features through its `[defines]` table so `moq.h`
wraps the gated declarations in `#ifdef MOQ_VIDEO` / `MOQ_AUDIO`. A
`[defines]` mapping only emits the guards; nothing enables them, so on its own
it would hide the codec API from every full-library consumer, OBS included.
`build.rs` therefore reads `CARGO_FEATURE_VIDEO` / `CARGO_FEATURE_AUDIO` and
writes `#define MOQ_VIDEO 1` / `#define MOQ_AUDIO 1` into the header's
`after_includes` block for each feature that is on. The full package's header
declares everything it does today with no consumer-side flag; the slim
package's header declares the subset. The `native-libs/*.txt` link lists that
the OBS build reads get a slim variant, or the recipe that emits them takes
the feature set.

`rs/libmoq/build.sh` takes `--no-default-features` like moq-ffi's, and
`libmoq.yml` runs it twice per target and uploads both assets. `doc/lib/c`
documents the variant and which symbols it lacks. A test builds the slim
library, asserts its `moq.h` declares none of the gated symbols, and links a
C program that opens a session and publishes a raw track, so the feature
gates cannot rot in either direction.

Public API: additive at the crate level (new features) and no change to the
full library's C ABI. The slim header is a subset.

## Required

- [Measure](/quest/m2/slim-bindings/measure.md) - the go or no-go for the line

## Related

- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the C ABI catch-up; a video decode knob landing there lands behind the `video` feature
