# [L] VAAPI encode and decode

## Goal

VAAPI encodes H.264 and H.265 from a DMA-BUF without a download, decodes
H.265 as well as H.264, and the `vaapi` feature costs a consumer nothing at
build time so it can return to default-on. Every piece needs a `moq-dev/vaapi`
release first.

## Plan

Three gaps, one external dependency.

**Decode.** The H.264 decoder landed (moq-vaapi 0.0.4, `decode/backend/vaapi.rs`),
with the default `decode::Config::output` of `Output::Native` handing out
DMA-BUF surfaces the renderer imports without a download. H.265 decode is still missing, so a Linux box
without NVDEC has no hardware path for it.

**The encoder.** H.264 is done. `moq-vaapi` imports DMA-BUFs and scales and
converts them through VPP (`dmabuf`, `vpp`, `Encoder::encode_dmabuf`,
`Encoder::set_bitrate`); `encode/backend/vaapi.rs` encodes a `Surface::DmaBuf`
without a download, and `Surface::resize` scales one through VPP. Validated on
Intel Meteor Lake. What is left is H.265, below.

**H.265.** The VAAPI backend advertises H.264 only. `moq-vaapi` 0.0.2 vendors
the HEVC buffer types (`src/buffer/hevc.rs`) but its `Encoder` is hardcoded to
`VAProfileH264Main` (with `VAEntrypointEncSlice`, or the low-power entrypoint
where that is all a device has), so exposing an HEVC encoder is a
change to that crate, not a flag here.

**Build cost.** `moq-vaapi` 0.0.3 dlopens libva (no `DT_NEEDED`), so a
libva-less host starts and `backend::open` falls through to the next encoder.
What remains is the build side: its build script runs bindgen over the
vendored libva headers, so every consumer needs libclang on the build host.
That is why `vaapi` is off by default while `nvidia` is on. Commit the
generated bindings to the crate and drop the build script and the bindgen
dependency, as `moq-nvenc` does: the output is portable (layout tests off,
fixed-width types, `c_char` left symbolic), so one checked-in file serves
every Linux target, as `moq-v4l` already does for `videodev2.h`. Then the
feature can return to default-on here.

Note what is already fine: a host with libva present but no usable VA driver
already falls back cleanly, since `Encoder::new` returns `Err` and
`backend::open` drops to openh264.

## Required

- A `moq-dev/vaapi` release exposing an HEVC encoder (H.264 decode is in
  0.0.4; DMA-BUF encode and VPP shipped in 0.1.0) and pre-generated bindings
  instead of a bindgen build script
