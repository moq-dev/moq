# [M] NVDEC and NVENC share CUDA arrays without a per-frame copy

## Goal

On a driver that supports NVIDIA Video Codec SDK 13.1's application-allocated CUDA arrays, moq-video's NVDEC decoder outputs into, and its NVENC encoder reads from, CUDA arrays the application owns, so a decoded frame reaches the encoder (or a renderer) without today's device-to-device copy per frame. A benchmark shows the saving at 1080p and 4K, and older drivers keep the copy path.

## Plan

Found by the NVENC vendor audit in [moq-dev/moq#7464](https://github.com/moq-dev/moq/pull/7464), which kept NVENC API 12.1 and recommended this as separate, measured work. Today `rs/moq-video/src/decode/backend/nvdec.rs` maps each decoded NV12 surface and copies it with `memcpy_dtod_sync` into an owned `cuda::Frame` (about line 409) before the surface is unmapped. SDK 13.1 lets the application allocate the CUarray input and output, and needs driver 610 or newer.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m3. It needs SDK 13.1 bindings and a driver-610 host to validate on, and no consumer has measured the copy as its bottleneck yet.

- Bindings: add the SDK 13.1 entry points to `rs/moq-nvenc` behind a runtime capability check, keeping 12.1 as the floor the crate loads against.
- Ownership: a bounded pool of CUDA arrays shared by decoder and encoder, with a frame holding its array until every consumer releases it, so the decoder can't overwrite a surface still being encoded.
- Synchronization: order decode completion before encode or render with CUDA events or stream ordering, not a blocking sync per frame.
- Validate on hardware: decoded pixels match the copy path, surface lifetime under drain and reset, and refusal (falling back to the copy) on an older driver. Measure frame-to-packet time and GPU memory bandwidth against the copy path; if it doesn't win, abandon the quest and report the numbers.

Public API: possibly a CUDA-array frame variant in moq-video's GPU frame types. Wire: none.

## Related

- [NVENC reuses its input and output buffers](/quest/m2/nvenc-pool.md) - per-frame allocation on the encode side, distinct from this copy
- [GPU CI](/quest/m2/gpu-ci.md) - the NVIDIA host this would validate on once its driver is new enough
