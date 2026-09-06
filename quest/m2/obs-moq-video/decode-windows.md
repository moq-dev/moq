# [L] Present moq-video decoded textures in OBS on Windows

## Goal

Supported Windows hardware decoders deliver GPU frames to the OBS source without CPU readback, with automatic CPU fallback and an accurate Stats indicator.

## Plan

- Extend the native frame contract from the source replacement using moq-video's D3D11 texture representation. Verify the decoder adapter/device, OBS graphics device, texture format, and sharing capabilities. Start with Media Foundation's actual native texture path; do not infer CUDA/D3D interop from handle types.
- Import compatible allocations or GPU-convert/blit into an OBS-compatible bounded pool. Model fences/keyed mutexes, render-thread ownership, device removal, resize, and release after GPU completion. An AddRef prevents object destruction but not decoder pool reuse.
- Test decoded color/timestamps and no-readback traces on real Windows hardware. Include incompatible adapters, hybrid GPU, unsupported formats, import failure, and stream restart. Fallback must preserve timing and expose its reason without silently reintroducing FFmpeg.

## Required

- [Video source replacement](/quest/m2/obs-moq-video/source.md) - shared native frame ownership, source rendering, and CPU fallback
