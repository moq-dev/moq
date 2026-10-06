# [M] VP8/VP9 in the OBS source

## Goal

The OBS MoQ source plays VP8 and VP9 (8-bit 4:2:0) on every platform it ships for, through moq-video's libvpx backend with libvpx statically linked, and no FFmpeg ABI or undeclared system codec library.

## Plan

- moq-video decodes both codecs behind its opt-in `vpx` feature, with libvpx supplied by the build host (`libvpx-native-sys`, `VPX_STATIC=1`). Turn it on in moq-ffi and libmoq for the OBS builds only, not for every binding artifact.
- macOS and Linux build through Nix, which already provides libvpx. The Windows MSVC build runs without Nix and needs a static libvpx: evaluate vcpkg's `libvpx:x64-windows-static` against an in-tree vendored libvpx build (C-only config first, SIMD later) and pick one. The vendored build is also what a crates.io consumer without libvpx would need.
- Verify decoded pixels in the source, CPU delivery, reconnect, and rendition changes, and inspect the plugin's imports for libvpx, avcodec, and avutil on each platform.

## Required

- [Video source replacement](/quest/m1/obs-moq-video/source.md) - the source decodes through moq-video first

## Related

- [Codec coverage](/quest/m2/video-codec-coverage.md) - hardware VP8/VP9 decode belongs to that review
