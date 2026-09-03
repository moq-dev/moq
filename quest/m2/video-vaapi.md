# [L] VAAPI encode and decode

## Goal

VAAPI encodes H.264 and H.265 from a DMA-BUF without a download, decodes at
all, and an always-on VAAPI binary loads on a host with no libva. Every piece
needs a `moq-dev/vaapi` release first.

## Plan

Four gaps, one external dependency.

**Decode.** There is no VAAPI decoder at all; the only Linux hardware decode
is NVDEC. iroh-live has a full stateless decoder with PRIME export, which is
the piece that pairs with DMA-BUF capture and the Vulkan renderer to make a
Linux path that never touches system memory.

**The encoder.** Ours is a 111-line CPU-only adapter whose own header says it
is unvalidated on hardware. iroh-live's imports a DMA-BUF directly and does
scale and convert through VPP, validated on Intel Meteor Lake. Take theirs and
reshape it to our surface rather than growing ours toward it.

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

- A `moq-dev/vaapi` release exposing an HEVC encoder, the decode half and a
  VPP wrapper, and restoring the `vaapi_dlopen` design
