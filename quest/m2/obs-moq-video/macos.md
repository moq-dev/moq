# [L] Feed OBS GPU frames into moq-video on macOS

## Goal

An OBS compositor frame reaches moq-video's VideoToolbox encoder without a GPU-to-CPU-to-GPU round trip, with bounded retained resources and correct color and timing.

## Plan

- Inspect OBS's OpenGL compositor, `encode_texture2`, and mac-videotoolbox input path. Determine whether the output allocation is IOSurface-backed and exportable. OBS's encoder currently copies CPU planes into its own pixel buffer; a CVPixelBuffer in moq-video alone does not remove that upstream readback.
- Prefer retained IOSurface/CVPixelBuffer storage in the format VideoToolbox accepts. Otherwise prototype GPU color conversion/blit into an IOSurface-backed NV12 pool. Specify GL/Metal/CoreVideo interop, graphics-context thread affinity, completion fences, and when OBS may recycle the source.
- Reuse `Surface::PixelBuffer` and the moq-video VideoToolbox backend. Retain the destination until encoding completes, including dropped submissions and cancellation. Bound the pool and handle resolution/HDR changes and device failure.
- Verify no CPU readback using GPU/API traces and copy counters. Compare direct import or GPU blit against the CPU baseline at 1080p60 and 4K where supported. Check decoded color bars and moving timestamps, latency percentiles, audio sync, stop/restart, and long-running pool reuse on Apple hardware.

## Required

- [Encoder adapter](/quest/m2/obs-moq-video/adapter.md) - frame ownership, queue policy, packet output, and comparison baseline
