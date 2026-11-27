# [S] Pre-cut Rust API fences for moq-audio and moq-opus

## Goal

Before cutting `0.22.0`, prevent three known immediate post-cut Rust semver breaks:

1. `moq_audio::playback::Device` (`rs/moq-audio/src/playback/device.rs`) is marked `#[non_exhaustive]` so adding an output `kind` field (headphones/headset vs speakers, needed by `moq-dev/studio` m0) is additive after `0.22.0`.
2. `moq_audio::decode::Read` (`rs/moq-audio/src/decode/consumer.rs`) is marked `#[non_exhaustive]` so [Native audio playout is a library](/quest/m3/audio-playout-library.md) can add an undecodable-packet variant without breaking `0.22.x`.
3. `moq-opus` (`rs/moq-opus/Cargo.toml`) exposes only the stable interface `moq-audio` needs before `0.22.0` (migrating `moq-audio` onto a stable control interface and fencing the raw c2rust `externs`, `varargs`, and re-exported raw entry points such as `opus_encoder_ctl_impl`), and stays exempted from the shared `0.22.0` minor in `version.json` if needed, so [safe Rust Opus](/quest/m3/opus-safe/README.md) can replace its c2rust internals without failing Nightly's crates.io API check during the additive phase.

## Plan

Decided 2026-11-26 (maintainer, pre-cut quest and consumer audit): ✅ fence these three API traps in one `[S]` `m2` quest before cutting `0.22.0` (rejected: also reshaping `moq_video::capture::App::source`, `moq_video::encode::Applied::cut`, and `moq-net` clean-end `Ok(None)` before `0.22.0`; leaving all fences for the next API cut).

Steps:

1. Add `#[non_exhaustive]` to `pub struct Device` in `rs/moq-audio/src/playback/device.rs` and `pub enum Read` in `rs/moq-audio/src/decode/consumer.rs`. Check every in-tree caller (including tests and `moq-cli` / `moq-ffi`) and update exhaustive struct destructuring patterns with `..` and exhaustive `Read` matches with `_`. Outside `moq-audio`, `#[non_exhaustive]` rejects struct literal construction of `Device`; check whether any external crate constructs `Device` and add a constructor or migration note if so.
2. Expose a stable control interface in `moq-opus` for `moq-audio`, migrate `moq-audio` off the raw `varargs!` / `opus_encoder_ctl_impl` calls onto it, and fence both the raw c2rust entry points (`opus_encoder_ctl_impl` and crate-root re-exports) and their `pub mod externs` / `varargs` modules before `0.22.0` so only the stable interface `moq-audio` needs is published on crates.io; adding `"moq-opus"` to `exemptions.rust` in `version.json` only affects version selection, so publishing the raw c2rust surface in `0.22` would still cause Nightly's API check against crates.io to fail when `opus-safe` replaces it during the additive phase. If `moq-opus` also stays on `0.1.x` in `version.json`, update `RELEASE.md`'s Versions section and confirm `just version` passes cleanly.
3. Run `just check` and verify the API diff reports the two `#[non_exhaustive]` additions cleanly.

Public API: breaking in `moq-audio` (`playback::Device` and `decode::Read` become `#[non_exhaustive]`). `moq-opus` stays exempt from the shared `0.22.0` minor. Wire: none.

## Related

- [Native audio playout is a library](/quest/m3/audio-playout-library.md) - adds the undecodable-packet `Read` variant
- [Safe Rust Opus](/quest/m3/opus-safe/README.md) - replaces `moq-opus`'s c2rust surface
