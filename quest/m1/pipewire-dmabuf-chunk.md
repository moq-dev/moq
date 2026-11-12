# [XS] PipeWire DMA-BUF capture ignores the chunk size

## Goal

PipeWire screen capture on Hyprland (xdg-desktop-portal-hyprland) delivers
DMA-BUF frames instead of dropping every one. That portal leaves `maxsize` at
0 and writes a placeholder chunk size of 9 on purpose, flagging a bad frame
with `SPA_CHUNK_FLAG_CORRUPTED` instead. The `pipewire.rs` tests also build
against PipeWire 1.0.x headers, as the library already does.

## Plan

Decided 2026-11-12 (maintainer, `/quest-import` of
[moq-dev/moq#8571](https://github.com/moq-dev/moq/issues/8571)):
✅ the fd's allocation size is the only bound (rejected: the reporter's
`dma_buf_maxsize_contains` patch, which keeps a second check that ignores the
offset and that the allocation check already covers; the same fix in m3).

In `rs/moq-video/src/capture/pipewire.rs`:

- Delete `dma_buf_chunk_contains` and `dma_buf_chunk_size`. The chunk size
  never reads a DMA-BUF: the GPU import and the CPU download go by the
  layout, the offset, and the fd's size from `dma_buf_allocation_size`, whose
  check (`offset + required <= allocation`) stays the bound. `chunk_kind`
  already treats a DMA-BUF chunk size of 0 as data; it keeps rejecting a
  CORRUPTED chunk.
- Add a pure-function test with Hyprland's shape: chunk size 9, `maxsize` 0,
  a 4K BGRx frame (33,177,600 bytes), an allocation that holds it. A CORRUPTED
  chunk is still rejected.
- The test that sets `spa::sys::SPA_VIDEO_TRANSFER_SMPTE2084` fails to build
  against PipeWire 1.0.x headers, which lack it. Spell it out as a local
  constant (14), the way
  [moq-dev/moq#3387](https://github.com/moq-dev/moq/pull/3387) did for
  `BT2020_10` and `BT601`.

Caused by [moq-dev/moq#2839](https://github.com/moq-dev/moq/pull/2839), which
added the chunk-size bound.

Public API: none. Wire: none.

## Hosts

- linux - PipeWire capture

## Closes

- [#8571](https://github.com/moq-dev/moq/issues/8571) - every DMA-BUF frame dropped on Hyprland

## Related

- [DMA-BUF capture into Vulkan](/quest/m4/2819-moq-video-carry-pipewire-dma-bufs-safely-into-the-vulkan.md) - the zero-copy path this unblocks on Hyprland
