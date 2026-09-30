# [L] VAAPI encode and decode

## Goal

VAAPI encodes H.264 and H.265 from a DMA-BUF without a download, decodes
H.265 as well as H.264, and the `vaapi` feature costs a consumer nothing at
build time so it can return to default-on. A resize reuses its output
surfaces.

## Plan

Decided in the 2026-09-30 audit: moved from m4, and the "gated on a
`moq-dev/vaapi` release" blocker is gone. That crate is our own repo (last
release 0.1.0 on 2026-09-24), so this quest includes the crate work (HEVC
encode, H.265 decode, pre-generated bindings, the resize pool) and cutting the
release, then bumping the workspace requirement here. The resize-pool quest
folds in for the same reason.

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

**Resize pool.** moq-vaapi's `Processor` allocates the blit output with
`ExportedFrame::from_surface` on every resize. Keep one surface per output
size; when the exported frame drops, the surface returns for the next blit of
that size. A frame the consumer still holds is not overwritten; the processor
allocates another. Keep at most one free surface per size and destroy any
returned past that, the decoder pool's rule. `Surface::resize` stays the same
call. The reuse test belongs in moq-vaapi; here, confirm a resize still returns
an NV12 DMA-BUF.

Note what is already fine: a host with libva present but no usable VA driver
already falls back cleanly, since `Encoder::new` returns `Err` and
`backend::open` drops to openh264.
