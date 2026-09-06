# [L] Feed OBS D3D11 frames into moq-video

## Goal

The moq-video OBS encoder consumes compositor output through D3D11 without CPU staging, and releases every shared texture and synchronization primitive correctly.

## Plan

- Inspect OBS `encoder_texture` shared handles and `encode_texture2` lock_key/next_key semantics. Record whether the source is packed RGB or split NV12 planes, its adapter LUID, and the lifetime OBS guarantees after the callback returns.
- Reuse `Surface::Texture` where the encoder accepts the same device and format. Otherwise GPU-convert/blit into a bounded encoder-owned NV12 pool before returning the OBS synchronization key. An AddRef alone does not prevent OBS from overwriting pooled pixels.
- Audit keyed mutex/fence sequencing, asynchronous completion, incompatible adapters, software fallback, resolution changes, and device removal. Start with Media Foundation, which accepts D3D11 surfaces. The current NVENC backend is Linux-only and directly imports CUDA surfaces there; Windows NVENC/D3D11 support is separate backend work. Do not infer interoperability from both APIs accepting a texture handle.
- Trace readback and GPU copy counts on real Windows hardware. Verify pixels and A/V timestamps with a subscriber, compare latency and utilization to the CPU baseline, and exercise cancellation while textures remain in flight. Include hybrid-GPU and device-mismatch rejection where available.

## Required

- [Encoder adapter](/quest/m2/obs-moq-video/adapter.md) - frame ownership, queue policy, packet output, and comparison baseline
