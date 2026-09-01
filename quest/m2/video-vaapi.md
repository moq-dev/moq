# [M] VAAPI encode gaps

## Goal

VAAPI encodes H.265, and an always-on VAAPI binary loads on a host with no
libva. Both need a `moq-dev/vaapi` release first.

## Plan

Two gaps, one external dependency.

**H.265.** The VAAPI backend advertises H.264 only. `moq-vaapi` 0.0.2 vendors
the HEVC buffer types (`src/buffer/hevc.rs`) but its `Encoder` is hardcoded to
`VAProfileH264Main` / `VAEntrypointEncSlice`, so exposing an HEVC encoder is a
change to that crate, not a flag here.

**Loading.** `moq-vaapi` 0.0.2 links libva rather than dlopening it: the
binary carries `NEEDED libva.so.2` and `libva-drm.so.2`, so a libva-less host
fails at process load, before any Rust probe could run. That is why the
NVENC-style driver probe cannot help here, and why `nvenc` and `vaapi` are
default-on opt-out features today so a self-compiler can drop the dependency.
The fix is restoring the documented `vaapi_dlopen` design in the external
crate (no `cargo:rustc-link-lib`, no `DT_NEEDED`), and only then adding the
probe on this side.

Note what is already fine: a host with libva present but no usable VA driver
already falls back cleanly, since `Encoder::new` returns `Err` and
`backend::open` drops to openh264.

## Required

- A `moq-dev/vaapi` release exposing an HEVC encoder and restoring the
  `vaapi_dlopen` design
