# [XL] Replace OBS source FFmpeg decoding with moq-video

## Goal

The MoQ source loads and plays supported video without directly linking FFmpeg libraries. Attempt macOS GPU delivery in the first implementation; automatically fall back to CPU delivery when native presentation is unavailable. Windows and Linux initially retain a portable CPU path.

## Plan

- Replace `cpp/obs/src/moq-source.cpp` video decode and conversion with moq-video through the generated C++ package: the moq-ffi video consumer for the CPU path, including frame ownership and cancellation semantics. Support H.264, HEVC, and available AV1 decoding; report unsupported VP8/VP9 explicitly until their follow-up lands.
- Decode with `MoqVideoDecoderOutput.native` set: each `MoqVideoDecodedFrame` retains the decoder's surface, `native()` borrows it (`PixelBuffer` on macOS) for as long as the frame lives, and `pixels(format)` is the CPU fallback. Hold the frame until OBS's GPU work reading it completes; held frames hold decoder pool slots.
- Implement the macOS presentation probe immediately: retain VideoToolbox PixelBuffer/IOSurface storage, inspect OBS graphics import and rendering support, and convert to OBS's expected color format on the GPU if necessary. Adapt the source render path to import textures on the graphics thread; preserve source timing instead of simply drawing the newest frame. An asynchronous CPU source API alone does not prove native GPU delivery.
- Bound decoded frames retained by the render thread. On import failure, switch to the existing I420 delivery path and show the reason in Stats. Keep fallback stable for the stream/device configuration rather than retrying every frame; re-probe on a relevant configuration change or restart. Device loss and resize must retire old surfaces only after rendering completes.
- Preserve timestamps, range/primaries, stride and plane layout, catalog/rendition changes, reconnect, visibility/deactivation behavior, and existing source settings. Carry frame-generation identity so late callbacks cannot display frames from a replaced source.
- Remove FFmpeg includes, CMake discovery/linkage, unit stubs, compile recipe requirements, and unused swresample linkage. Update OBS build/install docs and `doc/lib/cpp` together. libobs/Qt and native OS/GPU dependencies remain.
- Validate new code with decoded pixels and moving timestamps, GPU copy/readback traces, and p50/p95 decode-to-presentation delay. Exercise CPU fallback, unsupported codec, GPU import failure, device loss, repeated start/stop, rendition change, and delayed terminal completion. Verify no AVCodec/AVUtil/SWScale/SWResample imports using platform binary inspection. Load the artifact against the oldest supported OBS release and current stable release, using the repo's supported version policy at implementation time.

## Required

- [OBS migration](/quest/m1/cpp/obs.md) - the plugin is on the generated C++ before decode changes

## Related

- [VP8/VP9 decoding](/quest/m1/obs-moq-video/vpx.md) - restores deferred codec coverage independently
