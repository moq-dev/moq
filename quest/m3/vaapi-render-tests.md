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
The ignored `vaapi_1080p_ladder_stays_on_the_gpu` encode test in
`rs/moq-video/src/encode/backend/vaapi.rs` runs there too.

## Plan

Found by the m1 [coverage audit](/quest/m3/audit-test-coverage.md): no recipe
or quest named these tests. The m0 run mapped them to [GPU CI](/quest/m3/gpu-ci.md),
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
  [Video hardware validation](/quest/m5/video-hardware.md) and say so in its
  reason.
- Added 2026-10-27 (maintainer) from the coverage audit: convert the
  early-return VA-API decode and transcode tests to
  `#[ignore = "requires ..."]`, as [GPU CI](/quest/m3/gpu-ci.md) does for
  NVIDIA's `hw_available`, so hosted CI reports them ignored and this
  recipe's filter selects them.
- No hosted runner has these GPUs, so this stays a local recipe until a
  GPU runner exists ([GPU CI](/quest/m3/gpu-ci.md) owns that decision).
- Added 2026-11-06 (maintainer, /quest-unblock) from
  [moq-dev/moq#8013](https://github.com/moq-dev/moq/pull/8013)'s follow-ups:
  - The VA-API tests open the first suitable render node, so a second GPU
    is covered only when `Config::vaapi` names it, which moq-video no longer
    reads from the environment. `just rs gpu` runs the VA-API tests once for
    each AMD or Intel render node, passing it to the tests through a test-only
    knob ([env-knobs](/quest/m4/env-knobs.md) owns its name); a node of either vendor without its VA-API
    driver fails, and NVIDIA nodes are left to GPU CI. The renderer
    fixtures allocate their NV12 and I420 buffers on that node and pick the
    Vulkan adapter whose DRM render node matches it
    (`VK_EXT_physical_device_drm`), so each run stays on one GPU.
  - The H.265 decode tests gain an open-GOP stream with RASL pictures, and
    a Main 10 stream holding 8-bit samples that decodes. They also gain a
    10-bit stream, which is refused, since moq-vaapi has no P010 output
    ([moq-dev/vaapi#11](https://github.com/moq-dev/vaapi/pull/11)).
  - Not chosen: dropping either gap.
- Added 2026-11-08 (maintainer, approved follow-up): moq-vaapi's own
  tests return early too. `rs/moq-vaapi/src/encode.rs`, `decode.rs`, and
  `vpp.rs` print "skipping: ..." and pass when there is no VA-API display,
  no encoder or decoder for the codec, no VPP entrypoint, or no
  ffmpeg/ffprobe, so a host without libva or a driver reports them green. Give
  them the same `#[ignore = "requires ..."]` treatment and run them in
  `just rs gpu` per render node.

## Related

- [GPU CI](/quest/m3/gpu-ci.md) - the NVIDIA half of `just rs gpu`
- [VA-API encodes an external Vulkan image](/quest/m5/vaapi-vulkan-import.md) - the encode side of the same VA-API surfaces
