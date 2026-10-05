# [M] VA-API encodes an external Vulkan image

## Goal

An external [`Surface::Vulkan`](/quest/m2/gpu-surface.md) exported as a
DMA-BUF with a DRM format modifier, from a producer that synchronizes
explicitly through a timeline, encodes through VA-API on Intel (iHD) without a
CPU round trip. An ignored hardware test proves it on an Arrow Lake iGPU, and
`Kind::Auto` picks VA-API for a surface on an Intel device.

This is the by-design proof that the surface is not shaped around one vendor:
the same type reaches CUDA, Vulkan Video, and VA-API.

## Plan

Decided 2026-10-05 in a `/quest-plan` interview (paper trail in the PR that
added this quest): Intel uses VA-API, since ANV on Arrow Lake has no Vulkan
encode on Mesa 26.0.8 (it is in 26.2 behind `ANV_DEBUG`). H.264 is enough for
the proof; H.265 on VA-API belongs to [VAAPI encode and decode](/quest/m2/video-vaapi.md).

Today `encode/backend/vaapi.rs` takes a `Surface::DmaBuf` and waits for the
producer through an implicit-fence poll with a 500 ms timeout. It has no
timeline or sync_file import, so an explicit-sync producer races it.

- Synchronization: wait on the producer's ready value before VA-API reads the
  buffer, and signal the done value once the encode has read it, so slots
  recycle as they do for NVENC. Two candidates: a `vkWaitSemaphores` on a
  Vulkan device on the same GPU, or a sync_file exported from the timeline and
  imported into the DMA-BUF (`DMA_BUF_IOCTL_IMPORT_SYNC_FILE`) so the existing
  implicit path holds. Pick by what the test shows; neither may time out into
  an encode of a half-written buffer.
- Device: VA-API has no device UUID. Map the surface's UUID to a render node
  through `VK_EXT_physical_device_drm`, instead of moq-vaapi's default of the
  first render node (`MOQ_VAAPI_DEVICE` stays an override). On the test host
  the iGPU is `renderD128` and an AMD card is `renderD129`.
- Import the DMA-BUF with its modifier, and convert BGRA or RGBA to NV12
  through VPP. Refuse a modifier VA-API will not import rather than download.

Test: an ignored hardware test in `just rs gpu`'s Intel branch renders on the
iGPU with Vulkan, exports a DMA-BUF image and an explicit-sync timeline, and
encodes H.264 at two sizes; the output decodes, and the slots recycle.

Public API: none beyond the surface quest's. Wire: none.

## Required

- [One external GPU image for every encoder](/quest/m2/gpu-surface.md) - the surface VA-API imports

## Related

- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the AMD half of the same proof
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - H.265 encode on the same backend
- [Linux OBS GPU input](/quest/m3/obs-linux-gpu.md) - a DMA-BUF producer that needs the same explicit sync
- [Video hardware validation](/quest/m3/video-hardware.md) - VA-API so far ran only on Meteor Lake
