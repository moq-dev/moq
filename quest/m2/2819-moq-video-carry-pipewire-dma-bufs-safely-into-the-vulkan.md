# [XL] moq-video: carry PipeWire DMA-BUFs safely into the Vulkan renderer

## Goal

Implement and verify the behavior tracked in [#2819](https://github.com/moq-dev/moq/issues/2819)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Goal

Complete the Linux zero-copy surface spine from #2481 as one producer-to-consumer series:

`PipeWire DMA-BUF -> moq_video::Surface::DmaBuf -> Vulkan import -> render shader`

This is the first Linux producer and consumer for the public DMA-BUF surface contract. VAAPI encode/decode and V4L2 M2M can reuse the same contract afterward.

#### Required lifetime invariant

Duplicating a DMA-BUF fd preserves the allocation, not the pixels. Returning a dequeued PipeWire buffer lets the compositor overwrite that allocation while a queued frame or GPU import still reads it.

The PipeWire producer must therefore retain the dequeued buffer until the last `DmaBuf` clone drops, then return it to the stream on the PipeWire loop thread. The renderer must retain its clone until the GPU submission completes. An fd-only wrapper is racy and is not acceptable.

#### Phase 1: surface and producer

- \[x] Add the non-default `dmabuf` feature, enabled by `pipewire`, `vaapi`, and `render`.
- \[x] Add `Surface::DmaBuf` with a concrete public payload, typed DRM format, modifier, dimensions, plane offsets/strides, and mint-on-access `export() -> OwnedFd`.
- \[x] Keep backend export/download behavior private so no public implementable trait freezes backend internals.
- \[x] Negotiate DMA-BUF plus shared-memory fallback in PipeWire, preferring packed RGB for the first complete Vulkan path and retaining NV12 support.
- \[x] Retain the dequeued PipeWire buffer through the surface lifetime and return it on the loop thread.
- \[x] Keep the universal I420 fallback for linear NV12 and RGB DMA-BUFs. Reject non-linear CPU mapping instead of interpreting tiled memory as rows.
- \[x] Add unit coverage for descriptors, NV12 stride removal, buffer negotiation, and return-on-last-drop.

#### Phase 2: renderer consumer

- \[x] Add a Linux Vulkan DMA-BUF importer behind the non-default render feature.
- \[x] Import single-plane XRGB/ARGB/XBGR/ABGR through wgpu 30's `VULKAN_EXTERNAL_MEMORY_DMA_BUF` HAL path.
- \[x] Retain the producer surface until the GPU submission completes, so PipeWire cannot overwrite in-flight pixels.
- \[x] Preserve the renderer's CPU fallback and three-strike fast-path retirement.
- \[x] Document the wgpu device feature required for DMA-BUF import. A custom device-creation helper is unnecessary with wgpu 30.
- \[ ] Import multi-plane NV12 with explicit DRM modifier plane layouts.
- \[ ] Copy imported NV12 Y/UV planes into wgpu-sampleable R8/RG8 textures without touching the CPU.
- \[ ] Handle unsupported Intel tiling with a VAAPI VPP re-tile path. The current `moq-vaapi` API does not expose VPP yet.

#### Validation gates

- \[x] macOS `moq-video --all-features` compile and tests remain green.
- \[x] The wgpu 30 packed DMA-BUF HAL import compiles in an isolated Vulkan-enabled API check.
- \[x] Linux renderer-only cross-build (`x86_64-unknown-linux-gnu`, `--features render`).
- \[ ] Native Linux `--features pipewire,render` compile and tests.
- \[ ] Shared-memory PipeWire fallback still captures.
- \[ ] Intel or AMD desktop: packed DMA-BUF capture renders with zero CPU download.
- \[ ] Modifier mismatch exercises VPP re-tiling on hardware that needs it.
- \[ ] Holding several frames cannot produce torn/reused content or exhaust the pool permanently.
- \[ ] Hardware tests ship ignored with a reason where CI lacks the device.

The first packed-RGB vertical slice is implemented locally. Native Linux validation is still required before opening a PR, and the zero-copy tracker item stays open until the real hardware gates pass.

Refs #2481, #1837.

This also covers the Linux zero-copy capture input gap: V4L2 and PipeWire
convert to I420 on the CPU today (YUYV, BGRA), so V4L2 `VIDIOC_EXPBUF` export
and PipeWire DMA-BUF negotiation are what feed a `Surface::DmaBuf` straight
into VAAPI or NVENC.

## Closes

- [#2819](https://github.com/moq-dev/moq/issues/2819) - close this issue when the quest finishes

## Related

- [#2893: video: validate PipeWire DMA-BUF capture on KDE hardware](/quest/m3/2893-video-validate-pipewire-dma-buf-capture-on-kde-hardware.md) - related open work
