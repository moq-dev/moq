# [L] VAAPI encode and decode

## Goal

VAAPI encodes H.264 and H.265 from a DMA-BUF without a download, decodes
H.265 as well as H.264, and the `vaapi` feature costs a consumer nothing at
build time so it can return to default-on. Every piece needs a `moq-dev/vaapi`
release first.

## Plan

Four gaps, one external dependency.

**Decode.** The H.264 decoder landed (moq-vaapi 0.0.4, `decode/backend/vaapi.rs`),
with `decode::Config::gpu_frames` exporting DMA-BUF surfaces the renderer
imports without a download. H.265 decode is still missing, so a Linux box
without NVDEC has no hardware path for it.

**The encoder.** Ours is a 111-line CPU-only adapter whose own header says it
is unvalidated on hardware. iroh-live's imports a DMA-BUF directly and does
scale and convert through VPP, validated on Intel Meteor Lake. Take theirs and
reshape it to our surface rather than growing ours toward it.

**H.265.** The VAAPI backend advertises H.264 only. `moq-vaapi` 0.0.2 vendors
the HEVC buffer types (`src/buffer/hevc.rs`) but its `Encoder` is hardcoded to
`VAProfileH264Main` / `VAEntrypointEncSlice`, so exposing an HEVC encoder is a
change to that crate, not a flag here.

**Build cost.** `moq-vaapi` 0.0.3 dlopens libva (no `DT_NEEDED`), so a
libva-less host starts and `backend::open` falls through to the next encoder.
What remains is the build side: its build script runs bindgen over the
vendored libva headers, so every consumer needs libclang on the build host.
That is why `vaapi` is off by default while `nvidia` is on. Commit the
generated bindings to the crate and drop the build script and the bindgen
dependency, as `moq-nvenc` does: the output is portable (layout tests off,
fixed-width types, `c_char` left symbolic), so one checked-in file serves
every Linux target. Then the feature can return to default-on here. The
`v4l2` feature has the same shape, but its bindgen lives in the third-party
`v4l` crate, so that is a separate decision.

Note what is already fine: a host with libva present but no usable VA driver
already falls back cleanly, since `Encoder::new` returns `Err` and
`backend::open` drops to openh264.

## Required

- A `moq-dev/vaapi` release exposing an HEVC encoder, the decode half and a
  VPP wrapper, and shipping pre-generated bindings instead of a bindgen build
  script
