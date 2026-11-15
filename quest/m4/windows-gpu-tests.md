# [M] The Windows GPU tests run on a real GPU

## Goal

The ignored moq-video tests that need a Windows GPU, and that no CI runner
hosts, pass on the maintainer's `windows` host (Windows 11, NVIDIA RTX 2070),
and what a run breaks is fixed.

## Plan

Split from [Video hardware validation](/quest/m5/video-hardware.md) 2026-11-15
(maintainer, /quest-complete): these items need only that host's GPU, not the
cameras and other machines the rest of that quest waits on. Running ignored
tests changes no API, but a downstream studio's Windows run lists the parent
quest as related, so they sit in m4 rather than the lab.

- Direct3D11 video processing: `d3d11_resize_defaults_to_the_gpu`,
  `d3d11_resize_matches_cpu`, and
  `mediafoundation_resized_texture_reencodes_in_place`. `VideoProcessorBlt`
  can hang on affected drivers, so run them one at a time.
- WGC conversion and encode: `wgc_conversion_crops_odd_edges_and_preserves_color`,
  `wgc_conversion_workload`, and
  `wgc_nv12_encodes_with_matching_color_on_gpu_and_software`. The `wgc_*`
  capture tests belong to [Windows.Graphics.Capture](/quest/m3/capture-wgc.md).
- Media Foundation encode: `mediafoundation_cpu_rgba`.
  `mediafoundation_camera_texture` needs a camera the host lacks, so it stays
  in the parent quest.
- A GPU export on a hardware encoder:
  `render::export::dx12::tests::exports_an_nv12_texture` (the Platform runner
  has only WARP, which has no Direct3D11 video device) and
  `render::export::dx12::tests::media_foundation_encodes_the_export_in_place`.
  The [studio](https://github.com/moq-dev/studio), the export's main consumer,
  confirms in its own repository that its Output panel reports no readback on
  this machine.

Fix only a defect a run hits, and record the driver version each test ran on.
Running them here gives them no CI caller, so `sh/gh/test-inventory.allow`
keeps naming the parent quest as their owner.

Public API: none. Wire: none.

## Hosts

- windows - a Windows GPU with Direct3D11 video processing and a Media Foundation hardware H.264 encoder

## Related

- [Video hardware validation](/quest/m5/video-hardware.md) - the hardware paths that still wait on cameras and other machines
