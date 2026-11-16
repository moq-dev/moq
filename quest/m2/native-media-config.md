# [M] moq-video configs pick a device and a cursor mode with one typed field each

## Goal

Every moq-video config that opens hardware carries one `device` selector: a
typed value, present on every platform and feature set, honoured by whichever
backend opens and ignored otherwise, NVIDIA included. A caller sets it
without repeating moq-video's `cfg` gates or naming a backend.
`capture::Config::cursor` is an open enum (`Drawn`, `Hidden`) instead of a
bool, so a third mode is a new variant.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ One typed, always-present `device` selector per config (rejected: keep
  the backend-named path fields and only drop their `cfg` gates, which
  leaves NVIDIA unselectable; keep them and correct the crate doc). The
  reference user is a transcode host with more than one GPU, which cannot
  place NVIDIA work on a chosen GPU today.
- ✅ `capture::Config::cursor` becomes a `#[non_exhaustive]` enum with
  `Drawn` and `Hidden` (rejected: keep the bool and add a second field for
  the planned third mode). [The cursor apart from the frame](/quest/m4/capture-cursor.md)
  adds a mode the portal already has, and a bool plus a second field can
  say "drawn and also apart".

Facts found by the audit:

- [moq-dev/moq#8305](https://github.com/moq-dev/moq/pull/8305) replaced
  environment reads with fields, and left backend-named, `cfg`-gated paths
  on five public structs: `encode::Config`
  (`rs/moq-video/src/encode/encoder.rs:210`, `:215`), `decode::Config`
  (`rs/moq-video/src/decode/decoder.rs:94`, `:99`), `resize::Config`
  (`rs/moq-video/src/resize.rs:24`), `encode::Capture`
  (`rs/moq-video/src/encode/producer.rs:315`, `:318`), and
  `moq_transcode::Config` (`rs/moq-transcode/src/config.rs:61`, `:66`, `:71`).
- A field that exists only under
  `cfg(all(target_os = "linux", feature = "vaapi"))` makes every caller
  repeat the gate: `grep -rn 'feature = "vaapi"\|feature = "v4l2"'` finds 23
  lines in `rs/moq-cli/src` and 10 in `rs/moq-transcode/src`. A dependent
  crate has to mirror moq-video's feature names to compile.
- The crate doc promises the opposite: no public type, signature, or error
  variant names a backend (`rs/moq-video/src/lib.rs:58`).
- NVIDIA has no selector: NVDEC opens device 0
  (`rs/moq-video/src/decode/backend/nvdec.rs:140`), and NVENC opens device 0
  (`rs/moq-video/src/encode/backend/nvenc.rs:467`) unless `Config::input`, a
  `frame::vulkan::Device`, picks one by UUID (`:471`-`:478`).
- [GPU health](/quest/m3/gpu-health.md) already decided the vendor-neutral
  identity (2026-10-05): the render node, carried on `frame::vulkan::Device`.
- `pub cursor: bool` is at `rs/moq-video/src/capture/mod.rs:305`, with five
  readers: `capture/pipewire.rs:72`, `screencapture.rs:161`, `x11.rs:296`,
  `wgc/native.rs:263`, and `rs/moq-cli/src/publish.rs:1135`.

For the PR:

- Shape the selector as a `#[non_exhaustive]` enum, such as a DRM render
  node path and a V4L2 node path, defaulting to automatic choice. Settle in
  the PR how a config that needs two nodes says so (moq-transcode names a
  V4L2 decoder and a V4L2 encoder node separately today): one selector per
  direction is the likely answer, since decode and encode configs are
  already separate.
- NVIDIA maps a render node to its CUDA ordinal the way `Config::input`
  already maps a Vulkan device, so NVDEC and NVENC both honour it. Test it
  where two devices exist, or with the mapping function fed two fake
  identities; skip, as the other NVIDIA tests do, on a host with none.
- A named device that fails to open is an error, as a named VA-API node is
  today; only the automatic choice falls back.
- CLI flags keep their names (`--vaapi-device`, `--v4l2-encoder`,
  `--v4l2-decoder`, `--no-cursor`) and lose their `cfg` gates where the
  selector makes that possible.
- Recommended in the same PR, not decided by the maintainer: fence
  `capture::Mode` (`rs/moq-video/src/capture/mod.rs:203`), the one listing
  struct [moq-dev/moq#8742](https://github.com/moq-dev/moq/pull/8742) left
  without `#[non_exhaustive]`.
- Update `doc/lib/rs/moq-video.md` and `rs/moq-video/MIGRATE.md`.

Public API: moq-video and moq-transcode break (device fields on five
configs, `capture::Config::cursor`). CLI: none. Wire: none.

## Related

- [moq-video has a producer that takes frames](/quest/m2/video-producer.md) - changes `encode::Capture` and its driver in the same crate
- [GPU health](/quest/m3/gpu-health.md) - the render-node identity the selector reuses
- [The cursor apart from the frame](/quest/m4/capture-cursor.md) - adds the third cursor variant
