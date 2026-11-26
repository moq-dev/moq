# [S] Pre-cut Rust API fences for moq-audio and moq-opus

## Goal

Before cutting `0.22.0`, prevent three known immediate post-cut Rust semver breaks:

1. `moq_audio::playback::Device` (`rs/moq-audio/src/playback/device.rs`) is marked `#[non_exhaustive]` so adding an output `kind` field (headphones/headset vs speakers, needed by `moq-dev/studio` m0) is additive after `0.22.0`.
2. `moq_audio::decode::Read` (`rs/moq-audio/src/decode/consumer.rs`) is marked `#[non_exhaustive]` so [Native audio playout is a library](/quest/m3/audio-playout-library.md) can add an undecodable-packet variant without breaking `0.22.x`.
3. `moq-opus` (`rs/moq-opus/Cargo.toml`) is exempted from the shared `0.22.0` minor in `version.json` (staying on its `0.1.x` internal line, with `just version` passing) so [safe Rust Opus](/quest/m3/opus-safe/README.md) can replace its c2rust `externs`/`varargs` surface without forcing a breaking `0.23.0` bump across the workspace.

## Plan

Decided 2026-11-26 (maintainer, pre-cut quest and consumer audit): ✅ fence these three API traps in one `[S]` `m2` quest before cutting `0.22.0` (rejected: also reshaping `moq_video::capture::App::source`, `moq_video::encode::Applied::cut`, and `moq-net` clean-end `Ok(None)` before `0.22.0`; leaving all fences for the next API cut).

Steps:

1. Add `#[non_exhaustive]` to `pub struct Device` in `rs/moq-audio/src/playback/device.rs` and `pub enum Read` in `rs/moq-audio/src/decode/consumer.rs`. Check every in-tree caller (including tests and `moq-cli` / `moq-ffi`) and update struct literals or exhaustive matches to handle `..` / `_`.
2. Add `"moq-opus"` to `exemptions.rust` in `version.json` (and update `RELEASE.md`'s Versions section to document why `moq-opus` keeps its `0.1.x` line while `opus-safe` replaces the c2rust API), or hide its raw c2rust `pub mod externs` / `varargs` exports if `just version`'s promotion rule requires keeping it on the shared minor. Confirm `just version` passes cleanly.
3. Run `just check` and verify the API diff reports the two `#[non_exhaustive]` additions cleanly.

Public API: breaking in `moq-audio` (`playback::Device` and `decode::Read` become `#[non_exhaustive]`). `moq-opus` stays exempt from the shared `0.22.0` minor. Wire: none.

## Related

- [Native audio playout is a library](/quest/m3/audio-playout-library.md) - adds the undecodable-packet `Read` variant
- [Safe Rust Opus](/quest/m3/opus-safe/README.md) - replaces `moq-opus`'s c2rust surface
