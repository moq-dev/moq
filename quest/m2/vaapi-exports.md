# [S] moq-vaapi exports only what moq-video names

## Goal

The published `moq-vaapi` crate exports the items its one consumer,
moq-video, uses, not every constant and struct bindgen generates, and its
manifest describes the crate without "(AI GENERATED)".

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Narrow the re-exports and fix the description before the cut (rejected:
  keep as is and let the [vendor sync](/quest/m3/vendor-sync/vaapi.md) track
  upstream fixes only). Both are public API or metadata of a crate the cut
  publishes under the project's name.

Facts found by the audit:

- `rs/moq-vaapi/src/lib.rs:57` does `pub use bindings::*`, so all 22,389
  generated lines of `src/bindings/libva.rs` are public API of a published
  crate. Lines `:58`-`:68` glob-export nine more modules.
- moq-video is the only dependent, and names a short list
  (`grep -rn 'moq_vaapi::' rs/moq-video/src`): `Display`,
  `DrmDeviceIterator`, `Surface`, `Image`, `Codec`, `Color`,
  `UsageHint`, `DrmPrimeSurfaceDescriptor`, the `encode`, `decode`, `vpp`,
  and `dmabuf` modules, and seven `VA_FOURCC_*` and `VA_RT_FORMAT_*`
  constants.
- `rs/moq-vaapi/Cargo.toml:8` opens the description with "(AI GENERATED)".
  The crate is a fork of cros-libva and cros-codecs moved in-tree by
  [moq-dev/moq#8235](https://github.com/moq-dev/moq/pull/8235); the label
  reads as a statement about the whole release.

For the PR:

- Replace the globs with named exports, the raw bindings behind a
  crate-private module, and let the compiler find what moq-video still
  needs. Check Kixel and moq.pro for a direct `moq_vaapi` dependency first
  and keep what one uses.
- Rewrite the description to say what the crate is: VA-API H.264 and H.265
  encode and decode, forked from cros-libva and cros-codecs, with libva
  loaded at runtime.
- Not needed for the cut, since no dependent sees them, but cheap while
  here: `bindgen.sh` already fetches the libva headers by tag, so the roughly
  17.9k in-tree lines under `rs/moq-vaapi/libva/` can go once the tag is
  pinned in the script, and a function allowlist (the bindings declare 77
  `va*` functions and the crate calls about 35) would halve the generated
  file. Do them here only if they stay mechanical.

Public API: moq-vaapi breaks (already marked a break in the API diff).
Wire: none.

## Related

- [VA-API vendor sync](/quest/m3/vendor-sync/vaapi.md) - ports upstream fixes into the same crate
