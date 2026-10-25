# [XL] Replace OBS source FFmpeg video decoding with moq-video

## Goal

The MoQ source loads and plays supported video without FFmpeg's video libraries (libswscale, and libavcodec/libavutil for video). Attempt macOS GPU delivery in the first implementation; automatically fall back to CPU delivery when native presentation is unavailable. Windows and Linux initially retain a portable CPU path.

## Plan

- Replace `cpp/obs/src/moq-source.cpp` video decode and conversion with moq-video through the generated C++ package: the moq-ffi video consumer for the CPU path, including frame ownership and cancellation semantics. Support H.264, HEVC, and available AV1 decoding; report unsupported VP8/VP9 explicitly until their follow-up lands.
- Decode with `MoqVideoDecoderOutput.surface` set: each `MoqVideoDecodedFrame` retains the decoder's surface, `surface()` borrows it (`MoqVideoSurface::PixelBuffer` on macOS) for as long as the frame lives, and `pixels(format)` is the CPU fallback. A platform with no `MoqVideoSurface` variant refuses the opt-in. Hold the frame until OBS's GPU work reading it completes; held frames hold decoder pool slots. This surface is unreleased (#4094, renamed in 97575f002).
- Implement the macOS presentation probe immediately: retain VideoToolbox PixelBuffer/IOSurface storage, inspect OBS graphics import and rendering support, and convert to OBS's expected color format on the GPU if necessary. Adapt the source render path to import textures on the graphics thread; preserve source timing instead of simply drawing the newest frame. An asynchronous CPU source API alone does not prove native GPU delivery.
- Bound decoded frames retained by the render thread. On import failure, switch to the existing I420 delivery path and show the reason in Stats. Keep fallback stable for the stream/device configuration rather than retrying every frame; re-probe on a relevant configuration change or restart. Device loss and resize must retire old surfaces only after rendering completes.
- Preserve timestamps, range/primaries, stride and plane layout, catalog/rendition changes, reconnect, visibility/deactivation behavior, and existing source settings. Carry frame-generation identity so late callbacks cannot display frames from a replaced source.
- Remove the FFmpeg includes, CMake discovery/linkage (including the unused swresample), unit stubs, and compile recipe requirements; audio already decodes through moq-audio. Update OBS build/install docs and `doc/lib/cpp` together. libobs/Qt and native OS/GPU dependencies remain.
- Validate new code with decoded pixels and moving timestamps, GPU copy/readback traces, and p50/p95 decode-to-presentation delay. Exercise CPU fallback, unsupported codec, GPU import failure, device loss, repeated start/stop, rendition change, and delayed terminal completion. Verify no SWScale, AVCodec, AVUtil, or SWResample imports using platform binary inspection. Load the artifact against the oldest supported OBS release and current stable release, using the repo's supported version policy at implementation time.

The source consumes the `video` namespace decoder rather than the removed decoder on `BroadcastConsumer`.

The 2026-10-22 release audit reproduced two inherited correctness defects in
the current FFmpeg path at main 1d43c096e0. `moq_source_decode_frame` assigns
the current input packet timestamp to a decoded output, even when B-frame
reordering returns an older picture. The clean media EOF callback also
returns without draining delayed pictures. An algorithm-level reproduction
using real libavcodec and 12 H.264 B-frames produced 10 pictures, all with
incorrect assigned timestamps; explicit EOF draining recovered two omitted
pictures. This was not a running OBS integration test.
[OBS decode timing](/quest/m1/release-audit/obs-decode-timing.md) fixes both
on the current FFmpeg path in m1 (2026-10-22, maintainer); the replacement
keeps decoded output timestamps and clean EOF draining, covered with
reordered video.

## Related

- [VP8/VP9 in OBS](/quest/m2/obs-moq-video/vpx-obs.md) - restores deferred codec coverage once this lands
