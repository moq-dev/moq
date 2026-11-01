# [M] VA-API renderer tests

## Goal

The four ignored VA-API renderer import tests in
`rs/moq-video/src/render/renderer.rs` (`the_nv12_dmabuf_import_matches_the_cpu_path`,
`the_i420_dmabuf_import_matches_the_cpu_path`,
`decoded_frames_reach_the_gpu_without_a_download`, and
`recycled_decoder_surfaces_draw_the_latest_picture`) and
`packed_dmabuf_renders_through_vulkan` run in `just rs gpu` on AMD and Intel
hosts and fail there instead of skipping. So do the VA-API decode tests that
return early without a device today (`vaapi_h264_round_trip` and
`native_frames_still_answer_into_i420` in
`rs/moq-video/src/decode/backend/vaapi.rs`) and moq-transcode's
`vaapi_decoder_available` tests, which hosted CI reports as passed.

## Plan

Found by the m1 [coverage audit](/quest/m2/audit-test-coverage.md): no recipe
or quest named these tests. The m0 run mapped them to [GPU CI](/quest/m2/gpu-ci.md),
which covers only NVIDIA.

- `just rs gpu` already drives AMD and Intel through the flake's Mesa
  (`sh/rs/gpu.sh`). Run with that Mesa on 2026-10-20 (an Intel iGPU plus an
  AMD dGPU): the decode tests printed "skipping: no VA-API H.264
  decoder", I420 skipped for "this driver does not do YU12 DMA-BUFs", and
  NV12 failed with "no NV12 DMA-BUF could be allocated to import". Point
  libva at the flake's drivers (Mesa's radeonsi for AMD, intel-media-driver's
  iHD for Intel) and find out which of those remain real driver limits.
- Inside `just rs gpu` a missing driver is a failure, matching the rest of
  the recipe. A driver limit the test cannot work around (YU12 on one
  vendor) stays a skip that names it, outside the recipe's selection.
- `packed_dmabuf_renders_through_vulkan` needs a PipeWire desktop as well;
  if the recipe cannot provide one, leave it to
  [Video hardware validation](/quest/m3/video-hardware.md) and say so in its
  reason.
- Added 2026-10-27 (maintainer) from the coverage audit: convert the
  early-return VA-API decode and transcode tests to
  `#[ignore = "requires ..."]`, as [GPU CI](/quest/m2/gpu-ci.md) does for
  NVIDIA's `hw_available`, so hosted CI reports them ignored and this
  recipe's filter selects them.
- No hosted runner has these GPUs, so this stays a local recipe until a
  GPU runner exists ([GPU CI](/quest/m2/gpu-ci.md) owns that decision).

## Related

- [GPU CI](/quest/m2/gpu-ci.md) - the NVIDIA half of `just rs gpu`
- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - the encode side of the same VA-API surfaces
